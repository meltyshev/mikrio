export const MAX_FILE_BYTES = 1024 * 1024;
export const UPLOAD_LIFETIME_MS = 5 * 60 * 1000;
export const CLEANUP_RETRY_MS = 60 * 1000;
export const RECONCILIATION_PAGE_SIZE = 16;
export const SESSION_LIFETIME_MS = 8 * 60 * 60 * 1000;

export type ErrorCode =
  "invalid" | "unauthorized" | "notFound" | "conflict" | "unavailable";

export class AppError extends Error {
  constructor(
    readonly code: ErrorCode,
    options?: ErrorOptions,
  ) {
    super(code, options);
    this.name = "AppError";
  }
}

export interface Account {
  readonly accountId: string;
  readonly username: string;
  readonly salt: string;
  readonly passwordHash: string;
}

export interface Attempt {
  readonly attemptId: string;
  readonly accountId: string;
  readonly bucket: string;
  readonly key: string;
  readonly status: "pending" | "attached" | "abandoned";
  readonly filename: string;
  readonly contentLength: number;
  readonly sha256: string;
  readonly createdAt: number;
  readonly expiresAt: number;
}

export interface Attachment {
  readonly attachmentId: string;
  readonly filename: string;
  readonly contentLength: number;
  readonly sha256: string;
  readonly createdAt: number;
  readonly visibility: Visibility;
  readonly publication: "pending" | "applied" | "failed";
}

export type Visibility = "private" | "public";

export interface VisibilityWork {
  readonly attachmentId: string;
  readonly bucket: string;
  readonly key: string;
  readonly visibility: Visibility;
  readonly revision: number;
}

/** Durable ownership is independent of an upload attempt's retained history. */
export interface OwnedAttachment extends Attachment {
  readonly bucket: string;
  readonly key: string;
}

export interface StateOptions {
  path: string;
  storageId: string;
  bucket: string;
  prefix: string;
  initialize?: boolean;
}

/** The worker accepts only this app-owned protocol, never SQL from callers. */
export interface StateCommands {
  addAccount: { input: Account; output: void };
  account: { input: { username: string }; output: Account | undefined };
  addSession: {
    input: { accountId: string; tokenHash: string; now: number };
    output: void;
  };
  session: {
    input: { tokenHash: string; now: number };
    output: Pick<Account, "accountId" | "username"> | undefined;
  };
  logout: { input: { tokenHash: string }; output: void };
  createAttempt: { input: Attempt; output: void };
  attempt: {
    input: { attemptId: string; accountId: string };
    output: Attempt;
  };
  attach: {
    input: { attemptId: string; accountId: string; now: number };
    output: Attachment;
  };
  attachments: { input: { accountId: string }; output: readonly Attachment[] };
  attachment: {
    input: { attemptId: string; accountId: string };
    output: OwnedAttachment | undefined;
  };
  setVisibility: {
    input: { attemptId: string; accountId: string; visibility: Visibility };
    output: Attachment;
  };
  claimVisibility: { input: { now: number }; output: readonly string[] };
  visibilityWork: { input: { attemptId: string }; output: VisibilityWork };
  finishVisibility: {
    input: {
      attemptId: string;
      revision: number;
      succeeded: boolean;
      now: number;
    };
    output: void;
  };
  claimCleanup: { input: { now: number }; output: readonly Attempt[] };
  orphan: { input: { key: string }; output: boolean };
  compact: { input: undefined; output: number };
  close: { input: undefined; output: void };
}

/** Reject non-object input with the app's invalid-input error. */
export function parseRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new AppError("invalid");
  }

  return value as Record<string, unknown>;
}

/** Require nonempty, bounded UTF-8 metadata without control characters. */
export function parseText(value: unknown, maxBytes: number): string {
  if (typeof value !== "string") {
    throw new AppError("invalid");
  }

  const hasInvalidText =
    value.length === 0 ||
    !value.isWellFormed() ||
    Buffer.byteLength(value) > maxBytes ||
    // eslint-disable-next-line no-control-regex -- Reject literal control characters in app metadata.
    /[\u0000-\u001f\u007f]/u.test(value);

  if (hasInvalidText) {
    throw new AppError("invalid");
  }

  return value;
}

/** Require a nonnegative millisecond timestamp within the app's stored range. */
export function validateTimestamp(value: number): number {
  if (!Number.isSafeInteger(value) || value < 0 || value > 8_000_000_000_000) {
    throw new AppError("invalid");
  }

  return value;
}
