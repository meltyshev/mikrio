import { openSync, closeSync, statSync } from "node:fs";
import { DatabaseSync, type SQLOutputValue } from "node:sqlite";
import {
  AppError,
  CLEANUP_RETRY_MS,
  MAX_FILE_BYTES,
  SESSION_LIFETIME_MS,
  validateTimestamp,
  type Account,
  type Attachment,
  type Attempt,
  type OwnedAttachment,
  type StateOptions,
  type Visibility,
  type VisibilityWork,
} from "./model.mjs";

const APPLICATION_ID = 0x4d414154;

const SCHEMA = `
CREATE TABLE app (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  storage_id TEXT NOT NULL,
  bucket TEXT NOT NULL,
  prefix TEXT NOT NULL
) STRICT;
CREATE TABLE accounts (
  account_id TEXT PRIMARY KEY,
  username TEXT NOT NULL UNIQUE,
  salt TEXT NOT NULL CHECK (length(salt) = 32),
  password_hash TEXT NOT NULL CHECK (length(password_hash) = 64)
) STRICT;
CREATE TABLE sessions (
  token_hash TEXT PRIMARY KEY CHECK (length(token_hash) = 64),
  account_id TEXT NOT NULL REFERENCES accounts(account_id),
  expires_at INTEGER NOT NULL CHECK (expires_at >= 0)
) STRICT;
CREATE INDEX sessions_account ON sessions(account_id, expires_at);
CREATE INDEX sessions_expiry ON sessions(expires_at);
CREATE TABLE attempts (
  attempt_id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(account_id),
  bucket TEXT NOT NULL,
  object_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK (status IN ('pending', 'attached', 'abandoned')),
  filename TEXT NOT NULL,
  content_length INTEGER NOT NULL CHECK (content_length BETWEEN 1 AND 1048576),
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
  created_at INTEGER NOT NULL CHECK (created_at >= 0),
  expires_at INTEGER NOT NULL CHECK (expires_at > created_at),
  next_cleanup_at INTEGER,
  CHECK ((status = 'abandoned') = (next_cleanup_at IS NOT NULL))
) STRICT;
CREATE INDEX attempts_owner ON attempts(account_id, status);
CREATE INDEX attempts_expiry ON attempts(status, expires_at, attempt_id);
CREATE INDEX attempts_cleanup ON attempts(status, next_cleanup_at, attempt_id);
CREATE TABLE attachments (
  attempt_id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(account_id),
  bucket TEXT NOT NULL,
  object_key TEXT NOT NULL UNIQUE,
  filename TEXT NOT NULL,
  content_length INTEGER NOT NULL CHECK (content_length BETWEEN 1 AND 1048576),
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
  created_at INTEGER NOT NULL CHECK (created_at >= 0),
  desired_visibility TEXT NOT NULL DEFAULT 'private'
    CHECK (desired_visibility IN ('private', 'public')),
  visibility_revision INTEGER NOT NULL DEFAULT 1
    CHECK (visibility_revision BETWEEN 1 AND 9007199254740991),
  publication TEXT NOT NULL DEFAULT 'pending'
    CHECK (publication IN ('pending', 'applied', 'failed')),
  next_visibility_at INTEGER NOT NULL DEFAULT 0
    CHECK (next_visibility_at BETWEEN 0 AND 8000000000000)
) STRICT;
CREATE INDEX attachments_owner ON attachments(account_id, created_at, attempt_id);
CREATE INDEX attachments_visibility ON attachments(next_visibility_at, attempt_id);
`;

type Row = Record<string, SQLOutputValue>;

/** Synchronous SQL stays entirely on the database worker. */
export class Database {
  readonly #db: DatabaseSync;
  readonly #options: StateOptions;

  constructor(options: StateOptions) {
    this.#options = options;

    if (options.initialize) {
      // Never replace an existing database, including an interrupted initialization.
      closeSync(openSync(options.path, "wx", 0o600));
    } else if (!statSync(options.path).isFile()) {
      throw new AppError("unavailable");
    }

    this.#db = new DatabaseSync(options.path, {
      timeout: 2000,
      enableForeignKeyConstraints: true,
      enableDoubleQuotedStringLiterals: false,
      allowExtension: false,
    });

    try {
      this.#open(options);
    } catch (error) {
      try {
        this.#db.close();
      } catch (cleanup) {
        throw new AggregateError(
          [error, cleanup],
          "application database open and close failed",
          { cause: cleanup },
        );
      }

      throw error;
    }
  }

  close(): void {
    this.#db.close();
  }

  addAccount(account: Account): void {
    this.#runTransaction(() => {
      if (this.findAccount(account.username)) {
        throw new AppError("conflict");
      }

      this.#db
        .prepare(
          `INSERT INTO accounts (account_id, username, salt, password_hash)
        VALUES (?, ?, ?, ?)`,
        )
        .run(
          account.accountId,
          account.username,
          account.salt,
          account.passwordHash,
        );
    });
  }

  findAccount(username: string): Account | undefined {
    const row = this.#db
      .prepare(
        `SELECT account_id, username, salt, password_hash
      FROM accounts WHERE username = ?`,
      )
      .get(username);

    if (!row) {
      return undefined;
    }

    return {
      accountId: readString(row, "account_id"),
      username: readString(row, "username"),
      salt: readHex(row, "salt", 32),
      passwordHash: readHex(row, "password_hash", 64),
    };
  }

  addSession(accountId: string, tokenHash: string, now: number): void {
    validateTimestamp(now);

    this.#runTransaction(() => {
      this.#db.prepare("DELETE FROM sessions WHERE expires_at <= ?").run(now);

      this.#db
        .prepare(
          `DELETE FROM sessions WHERE token_hash IN (
        SELECT token_hash FROM sessions WHERE account_id = ?
        ORDER BY expires_at DESC, token_hash DESC LIMIT -1 OFFSET 7)`,
        )
        .run(accountId);

      this.#db
        .prepare(
          `INSERT INTO sessions (token_hash, account_id, expires_at)
        VALUES (?, ?, ?)`,
        )
        .run(tokenHash, accountId, now + SESSION_LIFETIME_MS);
    });
  }

  findSession(
    tokenHash: string,
    now: number,
  ): Pick<Account, "accountId" | "username"> | undefined {
    const row = this.#db
      .prepare(
        `SELECT a.account_id, a.username FROM sessions s
      JOIN accounts a USING (account_id) WHERE s.token_hash = ? AND s.expires_at > ?`,
      )
      .get(tokenHash, validateTimestamp(now));

    if (!row) {
      return undefined;
    }

    return {
      accountId: readString(row, "account_id"),
      username: readString(row, "username"),
    };
  }

  logout(tokenHash: string): void {
    this.#db
      .prepare("DELETE FROM sessions WHERE token_hash = ?")
      .run(tokenHash);
  }

  createAttempt(attempt: Attempt): void {
    if (attempt.bucket !== this.#options.bucket) {
      throw new AppError("invalid");
    }

    if (attempt.key !== this.#options.prefix + attempt.attemptId) {
      throw new AppError("invalid");
    }

    this.#runTransaction(() => {
      const attached = this.#db
        .prepare(
          "SELECT 1 FROM attachments WHERE attempt_id = ? OR object_key = ?",
        )
        .get(attempt.attemptId, attempt.key);

      if (attached) {
        throw new AppError("conflict");
      }

      const count = this.#db
        .prepare(
          `SELECT count(*) AS count FROM attempts
        WHERE account_id = ? AND status = 'pending'`,
        )
        .get(attempt.accountId)?.count;

      if (typeof count !== "number" || count >= 16) {
        throw new AppError("conflict");
      }

      this.#db
        .prepare(
          `INSERT INTO attempts (attempt_id, account_id, bucket, object_key, status,
          filename, content_length, sha256, created_at, expires_at, next_cleanup_at)
        VALUES (?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?, NULL)`,
        )
        .run(
          attempt.attemptId,
          attempt.accountId,
          attempt.bucket,
          attempt.key,
          attempt.filename,
          attempt.contentLength,
          attempt.sha256,
          validateTimestamp(attempt.createdAt),
          validateTimestamp(attempt.expiresAt),
        );
    });
  }

  getAttempt(attemptId: string, accountId: string): Attempt {
    const row = this.#db
      .prepare(
        `SELECT attempt_id, account_id, bucket, object_key, status, filename,
        content_length, sha256, created_at, expires_at
      FROM attempts WHERE attempt_id = ? AND account_id = ?`,
      )
      .get(attemptId, accountId);

    if (!row) {
      throw new AppError("notFound");
    }

    return decodeAttempt(row);
  }

  attach(attemptId: string, accountId: string, now: number): Attachment {
    return this.#runTransaction(() => {
      const attached = this.findAttachment(attemptId, accountId);

      if (attached) {
        return toAttachmentView(attached);
      }

      const attempt = this.getAttempt(attemptId, accountId);

      if (attempt.status === "abandoned") {
        throw new AppError("conflict");
      }

      if (attempt.status === "attached") {
        throw new AppError("unavailable");
      }

      if (attempt.expiresAt <= validateTimestamp(now)) {
        throw new AppError("conflict");
      }

      this.#db
        .prepare(
          `INSERT INTO attachments (attempt_id, account_id, bucket, object_key,
          filename, content_length, sha256, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
        )
        .run(
          attemptId,
          accountId,
          attempt.bucket,
          attempt.key,
          attempt.filename,
          attempt.contentLength,
          attempt.sha256,
          now,
        );

      this.#db
        .prepare("UPDATE attempts SET status = 'attached' WHERE attempt_id = ?")
        .run(attemptId);

      const result = this.findAttachment(attemptId, accountId);

      if (!result) {
        throw new AppError("unavailable");
      }

      return toAttachmentView(result);
    });
  }

  listAttachments(accountId: string): readonly Attachment[] {
    return this.#db
      .prepare(
        `SELECT attempt_id, bucket, object_key, filename, content_length, sha256,
        created_at, desired_visibility, publication FROM attachments WHERE account_id = ?
      ORDER BY created_at DESC, attempt_id DESC LIMIT 100`,
      )
      .all(accountId)
      .map((row) => toAttachmentView(decodeAttachment(row)));
  }

  findAttachment(
    attemptId: string,
    accountId: string,
  ): OwnedAttachment | undefined {
    const row = this.#db
      .prepare(
        `SELECT attempt_id, bucket, object_key, filename,
      content_length, sha256, created_at, desired_visibility, publication
      FROM attachments WHERE attempt_id = ? AND account_id = ?`,
      )
      .get(attemptId, accountId);

    return row ? decodeAttachment(row) : undefined;
  }

  setVisibility(
    attemptId: string,
    accountId: string,
    visibility: Visibility,
  ): Attachment {
    if (visibility !== "private" && visibility !== "public") {
      throw new AppError("invalid");
    }

    return this.#runTransaction(() => {
      const attached = this.findAttachment(attemptId, accountId);

      if (!attached) {
        throw new AppError("notFound");
      }

      if (attached.visibility === visibility) {
        return toAttachmentView(attached);
      }

      const changed = this.#db
        .prepare(
          `UPDATE attachments SET desired_visibility = ?,
        visibility_revision = visibility_revision + 1, publication = 'pending', next_visibility_at = 0
        WHERE attempt_id = ? AND account_id = ? AND visibility_revision < 9007199254740991`,
        )
        .run(visibility, attemptId, accountId).changes;

      if (changed !== 1) {
        throw new AppError("unavailable");
      }

      const current = this.findAttachment(attemptId, accountId);

      if (!current) {
        throw new AppError("unavailable");
      }

      return toAttachmentView(current);
    });
  }

  claimVisibility(now: number): readonly string[] {
    validateTimestamp(now);
    const retryAt = validateTimestamp(now + CLEANUP_RETRY_MS);

    return this.#runTransaction(() => {
      const attemptIds = this.#db
        .prepare(
          `SELECT attempt_id FROM attachments
        WHERE next_visibility_at <= ? ORDER BY next_visibility_at, attempt_id LIMIT 16`,
        )
        .all(now)
        .map((row) => readString(row, "attempt_id"));

      if (attemptIds.length === 0) {
        return attemptIds;
      }

      // BEGIN IMMEDIATE keeps this indexed selection identical to the one above.
      this.#db
        .prepare(
          `UPDATE attachments SET next_visibility_at = ? WHERE attempt_id IN (
        SELECT attempt_id FROM attachments WHERE next_visibility_at <= ?
        ORDER BY next_visibility_at, attempt_id LIMIT 16)`,
        )
        .run(retryAt, now);

      return attemptIds;
    });
  }

  getVisibilityWork(attemptId: string): VisibilityWork {
    const row = this.#db
      .prepare(
        `SELECT attempt_id, bucket, object_key,
      desired_visibility, visibility_revision FROM attachments WHERE attempt_id = ?`,
      )
      .get(attemptId);

    if (!row) {
      throw new AppError("notFound");
    }

    const revision = readInteger(row, "visibility_revision");

    if (revision < 1) {
      throw new AppError("unavailable");
    }

    return {
      attachmentId: readString(row, "attempt_id"),
      bucket: readString(row, "bucket"),
      key: readString(row, "object_key"),
      visibility: decodeVisibility(row),
      revision,
    };
  }

  finishVisibility(
    attemptId: string,
    revision: number,
    succeeded: boolean,
    now: number,
  ): void {
    const retryAt = validateTimestamp(now + CLEANUP_RETRY_MS);

    this.#runTransaction(() => {
      // An old acknowledgment cannot clear a newer durable intent. Even applied
      // rows recur: a timed-out older remote write may still finish afterward.
      this.#db
        .prepare(
          `UPDATE attachments SET publication = ?, next_visibility_at = ?
        WHERE attempt_id = ? AND visibility_revision = ?`,
        )
        .run(succeeded ? "applied" : "failed", retryAt, attemptId, revision);
    });
  }

  claimCleanup(now: number): readonly Attempt[] {
    validateTimestamp(now);

    return this.#runTransaction(() => {
      // Bound each transaction. Repeated scans permit separate terminal compaction.
      this.#db
        .prepare(
          `UPDATE attempts SET status = 'abandoned', next_cleanup_at = ?
        WHERE attempt_id IN (SELECT attempt_id FROM attempts
        WHERE status = 'pending' AND expires_at <= ? ORDER BY expires_at, attempt_id LIMIT 16)`,
        )
        .run(now, now);

      const due = this.#db
        .prepare(
          `SELECT attempt_id, account_id, bucket, object_key, status, filename,
          content_length, sha256, created_at, expires_at
        FROM attempts WHERE status = 'abandoned'
        AND next_cleanup_at <= ? ORDER BY next_cleanup_at, attempt_id LIMIT 16`,
        )
        .all(now)
        .map(decodeAttempt);

      if (due.length === 0) {
        return due;
      }

      this.#db
        .prepare(
          `UPDATE attempts SET next_cleanup_at = ? WHERE attempt_id IN (
        SELECT attempt_id FROM attempts WHERE status = 'abandoned' AND next_cleanup_at <= ?
        ORDER BY next_cleanup_at, attempt_id LIMIT 16)`,
        )
        .run(now + CLEANUP_RETRY_MS, now);

      return due;
    });
  }

  isOrphan(key: string): boolean {
    if (!key.startsWith(this.#options.prefix)) {
      throw new AppError("invalid");
    }

    // One statement observes both current histories and durable live ownership.
    const row = this.#db
      .prepare(
        `SELECT NOT EXISTS (SELECT 1 FROM attempts WHERE object_key = ?
        AND status != 'abandoned') AND NOT EXISTS (
        SELECT 1 FROM attachments WHERE object_key = ?) AS orphan`,
      )
      .get(key, key);

    if (row?.orphan !== 0 && row?.orphan !== 1) {
      throw new AppError("unavailable");
    }

    return row.orphan === 1;
  }

  compact(): number {
    return this.#runTransaction(() =>
      Number(
        this.#db
          .prepare(
            `DELETE FROM attempts
      WHERE attempt_id IN (SELECT a.attempt_id FROM attempts a
      WHERE status = 'abandoned' AND NOT EXISTS (
        SELECT 1 FROM attachments t WHERE t.object_key = a.object_key
          OR t.attempt_id = a.attempt_id)
      ORDER BY next_cleanup_at, attempt_id LIMIT 16)`,
          )
          .run().changes,
      ),
    );
  }

  #open(options: StateOptions): void {
    if (!options.initialize) {
      const applicationId = this.#db
        .prepare("PRAGMA application_id")
        .get()?.application_id;

      if (applicationId !== APPLICATION_ID) {
        throw new AppError("unavailable");
      }

      const version = Number(
        this.#db.prepare("PRAGMA user_version").get()?.user_version,
      );

      if (version !== 1) {
        throw new AppError("unavailable");
      }

      const app = this.#db
        .prepare(
          "SELECT storage_id, bucket, prefix FROM app WHERE singleton = 1",
        )
        .get();

      const hasMismatchedIdentity =
        app?.storage_id !== options.storageId ||
        app.bucket !== options.bucket ||
        app.prefix !== options.prefix;

      if (hasMismatchedIdentity) {
        throw new AppError("unavailable");
      }
    }

    this.#db.exec("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;");

    const journalMode = this.#db
      .prepare("PRAGMA journal_mode")
      .get()?.journal_mode;

    if (journalMode !== "wal") {
      throw new AppError("unavailable");
    }

    const synchronous = this.#db
      .prepare("PRAGMA synchronous")
      .get()?.synchronous;

    if (synchronous !== 2) {
      throw new AppError("unavailable");
    }

    const foreignKeys = this.#db
      .prepare("PRAGMA foreign_keys")
      .get()?.foreign_keys;

    if (foreignKeys !== 1) {
      throw new AppError("unavailable");
    }

    if (options.initialize) {
      this.#runTransaction(() => {
        this.#db.exec(SCHEMA);

        this.#db
          .prepare(
            `INSERT INTO app (singleton, storage_id, bucket, prefix)
          VALUES (1, ?, ?, ?)`,
          )
          .run(options.storageId, options.bucket, options.prefix);

        this.#db.exec(
          `PRAGMA application_id = ${APPLICATION_ID}; PRAGMA user_version = 1;`,
        );
      });
    }

    const quickCheck = this.#db
      .prepare("PRAGMA quick_check")
      .get()?.quick_check;

    if (quickCheck !== "ok") {
      throw new AppError("unavailable");
    }

    const foreignKeyViolation = this.#db
      .prepare("PRAGMA foreign_key_check")
      .get();

    if (foreignKeyViolation !== undefined) {
      throw new AppError("unavailable");
    }

    const inconsistentAttachment = this.#db
      .prepare(
        `SELECT 1 FROM attempts a LEFT JOIN attachments t USING (attempt_id)
        WHERE (a.status = 'attached') != (t.attempt_id IS NOT NULL)
        OR (t.attempt_id IS NOT NULL AND (a.account_id != t.account_id
          OR a.bucket != t.bucket OR a.object_key != t.object_key
          OR a.filename != t.filename OR a.content_length != t.content_length
          OR a.sha256 != t.sha256)) LIMIT 1`,
      )
      .get();

    if (inconsistentAttachment !== undefined) {
      throw new AppError("unavailable");
    }
  }

  #runTransaction<T>(action: () => T): T {
    this.#db.exec("BEGIN IMMEDIATE");

    try {
      const result = action();
      this.#db.exec("COMMIT");
      return result;
    } catch (error) {
      // A COMMIT error is not reported as rollback proof. Callers reconcile by ID.
      try {
        if (this.#db.isTransaction) {
          this.#db.exec("ROLLBACK");
        }
      } catch (cleanup) {
        throw new AggregateError(
          [error, cleanup],
          "application transaction and rollback failed",
          { cause: cleanup },
        );
      }

      throw error;
    }
  }
}

function readString(row: Row, name: string): string {
  const value = row[name];

  if (typeof value !== "string") {
    throw new AppError("unavailable");
  }

  return value;
}

function readInteger(row: Row, name: string): number {
  const value = row[name];

  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new AppError("unavailable");
  }

  return value;
}

function readHex(row: Row, name: string, length: number): string {
  const value = readString(row, name);

  if (value.length !== length || !/^[0-9a-f]+$/u.test(value)) {
    throw new AppError("unavailable");
  }

  return value;
}

function decodeAttempt(row: Row): Attempt {
  const status = readString(row, "status");
  const contentLength = readInteger(row, "content_length");

  const hasInvalidStatus =
    status !== "pending" && status !== "attached" && status !== "abandoned";

  if (hasInvalidStatus) {
    throw new AppError("unavailable");
  }

  if (contentLength < 1 || contentLength > MAX_FILE_BYTES) {
    throw new AppError("unavailable");
  }

  return {
    attemptId: readString(row, "attempt_id"),
    accountId: readString(row, "account_id"),
    bucket: readString(row, "bucket"),
    key: readString(row, "object_key"),
    status,
    filename: readString(row, "filename"),
    contentLength,
    sha256: readHex(row, "sha256", 64),
    createdAt: readInteger(row, "created_at"),
    expiresAt: readInteger(row, "expires_at"),
  };
}

function decodeAttachment(row: Row): OwnedAttachment {
  const contentLength = readInteger(row, "content_length");

  if (contentLength < 1 || contentLength > MAX_FILE_BYTES) {
    throw new AppError("unavailable");
  }

  const publication = readString(row, "publication");

  const hasInvalidPublication =
    publication !== "pending" &&
    publication !== "applied" &&
    publication !== "failed";

  if (hasInvalidPublication) {
    throw new AppError("unavailable");
  }

  return {
    attachmentId: readString(row, "attempt_id"),
    bucket: readString(row, "bucket"),
    key: readString(row, "object_key"),
    filename: readString(row, "filename"),
    contentLength,
    sha256: readHex(row, "sha256", 64),
    createdAt: readInteger(row, "created_at"),
    visibility: decodeVisibility(row),
    publication,
  };
}

function toAttachmentView(attachment: OwnedAttachment): Attachment {
  return {
    attachmentId: attachment.attachmentId,
    filename: attachment.filename,
    contentLength: attachment.contentLength,
    sha256: attachment.sha256,
    createdAt: attachment.createdAt,
    visibility: attachment.visibility,
    publication: attachment.publication,
  };
}

function decodeVisibility(row: Row): Visibility {
  const value = readString(row, "desired_visibility");

  if (value !== "private" && value !== "public") {
    throw new AppError("unavailable");
  }

  return value;
}
