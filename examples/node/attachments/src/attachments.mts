import { randomUUID } from "node:crypto";
import {
  AppError,
  MAX_FILE_BYTES,
  RECONCILIATION_PAGE_SIZE,
  UPLOAD_LIFETIME_MS,
  parseRecord,
  parseText,
  type Attachment,
  type Attempt,
} from "./model.mjs";
import type { ObjectLocation, ObjectStorage, Upload } from "./storage.mjs";
import type { State } from "./state.mjs";

/** Verify and attach permanent keys; cleanup requires accepted app authority. */
export class Attachments {
  #cleanup: Promise<Readonly<{ deleted: number; failed: number }>> | undefined;
  #continuationToken: string | undefined;

  constructor(
    readonly state: State,
    readonly storage: ObjectStorage,
    readonly bucket: string,
    readonly prefix: string,
    readonly authoritativeState: boolean,
  ) {}

  async create(
    accountId: string,
    input: unknown,
  ): Promise<
    Readonly<{ attemptId: string; expiresAt: number; upload: Upload }>
  > {
    const fields = parseRecord(input);
    const filename = parseText(fields.filename, 255);
    const sha256 = parseText(fields.sha256, 64);
    const contentLength = fields.contentLength;

    if (!/^[0-9a-f]{64}$/u.test(sha256)) {
      throw new AppError("invalid");
    }

    if (typeof contentLength !== "number") {
      throw new AppError("invalid");
    }

    const hasInvalidLength =
      !Number.isSafeInteger(contentLength) ||
      contentLength < 1 ||
      contentLength > MAX_FILE_BYTES;

    if (hasInvalidLength) {
      throw new AppError("invalid");
    }

    const hasUnknownFields = Object.keys(fields).some(
      (key) => !["filename", "sha256", "contentLength"].includes(key),
    );

    if (hasUnknownFields) {
      throw new AppError("invalid");
    }

    const attemptId = randomUUID();
    const now = Date.now();

    const attempt: Attempt = {
      attemptId,
      accountId,
      bucket: this.bucket,
      key: this.prefix + attemptId,
      status: "pending",
      filename,
      contentLength,
      sha256,
      createdAt: now,
      expiresAt: now + UPLOAD_LIFETIME_MS,
    };

    // A failed/uncertain commit never issues a URL. A retained row can still expire.
    await this.state.call("createAttempt", attempt);
    const upload = await this.storage.upload(attempt);
    return { attemptId, expiresAt: attempt.expiresAt, upload };
  }

  async finalize(accountId: string, attemptId: string): Promise<Attachment> {
    const attached = await this.state.call("attachment", {
      attemptId,
      accountId,
    });

    if (attached) {
      this.#validateLocation(attached, attemptId);

      return this.state.call("attach", {
        attemptId,
        accountId,
        now: Date.now(),
      });
    }

    const attempt = await this.state.call("attempt", { attemptId, accountId });
    this.#validateLocation(attempt, attemptId);

    if (attempt.status === "abandoned") {
      throw new AppError("conflict");
    }

    if (attempt.status === "attached") {
      return this.state.call("attach", {
        attemptId,
        accountId,
        now: Date.now(),
      });
    }

    if (attempt.expiresAt <= Date.now()) {
      throw new AppError("conflict");
    }

    await this.storage.verify(attempt);

    // No SQL transaction spans GET. Cleanup may have won while verification ran.
    return this.state.call("attach", { attemptId, accountId, now: Date.now() });
  }

  async download(accountId: string, attemptId: string): Promise<string> {
    const attachment = await this.state.call("attachment", {
      attemptId,
      accountId,
    });

    if (!attachment) {
      throw new AppError("notFound");
    }

    this.#validateLocation(attachment, attemptId);
    return this.storage.download(attachment);
  }

  cleanup(
    now = Date.now(),
  ): Promise<Readonly<{ deleted: number; failed: number }>> {
    if (!this.authoritativeState) {
      return Promise.reject(new AppError("unavailable"));
    }

    this.#cleanup ??= this.#clean(now)
      .catch((error: unknown) => {
        // Tokens are only traversal hints. Restart after an outage or stale token.
        this.#continuationToken = undefined;
        throw error;
      })
      .finally(() => {
        this.#cleanup = undefined;
      });

    return this.#cleanup;
  }

  async #clean(
    now: number,
  ): Promise<Readonly<{ deleted: number; failed: number }>> {
    const attempts = await this.state.call("claimCleanup", { now });
    let deleted = 0;
    let failed = 0;

    for (const candidate of attempts) {
      // A fresh authoritative read precedes each destructive request. DB outages
      // stop the batch; SDK failures retry through retained history or later scans.
      this.#validateLocation(candidate, candidate.attemptId);

      if (!(await this.state.call("orphan", { key: candidate.key }))) {
        continue;
      }

      try {
        await this.storage.delete(candidate);
        deleted += 1;
      } catch {
        failed += 1;
      }
    }

    const page = await this.storage.list(
      this.bucket,
      this.prefix,
      this.#continuationToken,
    );

    if (page.keys.length > RECONCILIATION_PAGE_SIZE) {
      throw new AppError("unavailable");
    }

    if (page.keys.some((key) => !key.startsWith(this.prefix))) {
      throw new AppError("unavailable");
    }

    const hasInvalidContinuation =
      page.nextContinuationToken !== undefined &&
      (!page.nextContinuationToken ||
        page.nextContinuationToken === this.#continuationToken);

    if (hasInvalidContinuation) {
      throw new AppError("unavailable");
    }

    const attempted = new Set(attempts.map((attempt) => attempt.key));

    for (const key of page.keys) {
      if (attempted.has(key)) {
        continue;
      }

      if (!(await this.state.call("orphan", { key }))) {
        continue;
      }

      try {
        await this.storage.delete({ bucket: this.bucket, key });
        deleted += 1;
      } catch {
        failed += 1;
      }
    }

    this.#continuationToken = page.nextContinuationToken;

    // Only a completed authorized traversal enables this bounded compaction.
    // Subsequent passes start again, discovering late writes behind this cursor.
    if (this.#continuationToken === undefined) {
      await this.state.call("compact", undefined);
    }

    return { deleted, failed };
  }

  #validateLocation(object: ObjectLocation, attemptId: string): void {
    if (object.bucket !== this.bucket) {
      throw new AppError("unavailable");
    }

    if (object.key !== this.prefix + attemptId) {
      throw new AppError("unavailable");
    }
  }
}
