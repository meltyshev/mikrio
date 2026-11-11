import type {
  MutationOutcome,
  DeleteObjectsProgress,
  DeleteObjectProgress,
  DeleteError,
} from "./values.cjs";

/** A domain failure; an absent outcome never establishes mutation rollback. */
export class StorageError extends Error {
  readonly code: string;
  readonly mutationOutcome?: MutationOutcome | undefined;
  readonly partialResult?: DeleteObjectsProgress | undefined;

  constructor(
    code: string,
    message: string,
    options?: ErrorOptions,
    outcome?: MutationOutcome,
    partialResult?: DeleteObjectsProgress,
  ) {
    super(message, options);
    this.name = "StorageError";
    this.code = code;
    this.mutationOutcome = outcome;
    this.partialResult = partialResult;
  }
}

export function storageError(error: unknown): unknown {
  if (error instanceof StorageError) {
    return error;
  }

  if (!(error instanceof Error)) {
    return error;
  }

  if (!("code" in error) || typeof error.code !== "string") {
    return error;
  }

  const outcome =
    "mutationOutcome" in error && isOutcome(error.mutationOutcome)
      ? error.mutationOutcome
      : undefined;

  return new StorageError(
    error.code,
    error.message,
    { cause: error.cause },
    outcome,
    "partialResult" in error ? progress(error.partialResult) : undefined,
  );
}

function progress(value: unknown): DeleteObjectsProgress | undefined {
  if (typeof value !== "object" || value === null) {
    return undefined;
  }

  if (!("items" in value) || !Array.isArray(value.items)) {
    return undefined;
  }

  if (value.items.length > 1000) {
    return undefined;
  }

  const items: DeleteObjectProgress[] = [];

  for (const item of value.items as unknown[]) {
    if (typeof item !== "object" || item === null) {
      return undefined;
    }

    if (!("index" in item) || item.index !== items.length) {
      return undefined;
    }

    if (!("key" in item) || typeof item.key !== "string") {
      return undefined;
    }

    if (!("mutationOutcome" in item)) {
      return undefined;
    }

    if (!isOutcome(item.mutationOutcome)) {
      return undefined;
    }

    if (item.mutationOutcome === "Partial") {
      return undefined;
    }

    let error: DeleteError | undefined;

    if ("error" in item && item.error !== undefined) {
      const detail = item.error;

      if (typeof detail !== "object" || detail === null) {
        return undefined;
      }

      if (!("code" in detail) || typeof detail.code !== "string") {
        return undefined;
      }

      if (!("message" in detail) || typeof detail.message !== "string") {
        return undefined;
      }

      error = Object.freeze({ code: detail.code, message: detail.message });
    }

    items.push(
      Object.freeze({
        index: items.length,
        key: item.key,
        mutationOutcome: item.mutationOutcome,
        error,
      }),
    );
  }

  return Object.freeze({ items: Object.freeze(items) });
}

function isOutcome(value: unknown): value is MutationOutcome {
  return (
    value === "NotCommitted" ||
    value === "Committed" ||
    value === "Unknown" ||
    value === "Partial"
  );
}

export function canceled(
  signal: AbortSignal,
  outcome?: MutationOutcome,
  partialResult?: DeleteObjectsProgress,
): StorageError {
  return new StorageError(
    "OperationCanceled",
    "Operation was canceled",
    { cause: signal.reason },
    outcome,
    partialResult,
  );
}

export function beforeMutation(error: unknown): unknown {
  const converted = storageError(error);

  if (!(converted instanceof StorageError)) {
    return converted;
  }

  if (converted.mutationOutcome !== undefined) {
    return converted;
  }

  return new StorageError(
    converted.code,
    converted.message,
    { cause: converted.cause },
    "NotCommitted",
    converted.partialResult,
  );
}
