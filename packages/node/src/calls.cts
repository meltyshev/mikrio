import { canceled, storageError, StorageError } from "./errors.cjs";
import { record } from "./inputs.cjs";
import { loadNative, type NativeCancellation } from "./native.cjs";
import type { CallOptions } from "./index.cjs";

export function callSignal(value: unknown): AbortSignal | undefined {
  if (value === undefined) {
    return undefined;
  }

  const input = record(value, ["signal"], "CallOptions");

  if (input.signal !== undefined && !(input.signal instanceof AbortSignal)) {
    throw new TypeError("signal must be an AbortSignal or undefined");
  }

  return input.signal;
}

/** One owned signal registration. GET transfers it to its returned body. */
export class Call {
  readonly cancellation: NativeCancellation;
  readonly signal: AbortSignal | undefined;
  #onCancel: (() => void) | undefined;

  readonly #abort = (): void => {
    this.cancellation.cancel();
    this.#onCancel?.();
  };

  constructor(options: CallOptions | undefined, mutation: boolean) {
    this.signal = callSignal(options);

    if (this.signal?.aborted) {
      throw canceled(this.signal, mutation ? "NotCommitted" : undefined);
    }

    this.cancellation = new (loadNative().NativeCancellation)();
    this.signal?.addEventListener("abort", this.#abort, { once: true });

    if (this.signal?.aborted) {
      this.#abort();
    }
  }

  onCancel(callback: () => void): void {
    this.#onCancel = callback;

    if (this.signal?.aborted) {
      callback();
    }
  }

  close(): void {
    this.signal?.removeEventListener("abort", this.#abort);
    this.#onCancel = undefined;
  }
}

export async function invoke<T>(
  options: CallOptions | undefined,
  mutation: boolean,
  run: (cancellation: NativeCancellation) => Promise<T>,
): Promise<T> {
  const call = new Call(options, mutation);

  try {
    return await run(call.cancellation);
  } catch (error: unknown) {
    const converted = storageError(error);

    if (converted instanceof StorageError) {
      const outcome =
        converted.mutationOutcome ?? (mutation ? "Unknown" : undefined);

      if (converted.code === "OperationCanceled" && call.signal?.aborted) {
        throw canceled(call.signal, outcome, converted.partialResult);
      }

      if (outcome !== converted.mutationOutcome) {
        throw new StorageError(
          converted.code,
          converted.message,
          { cause: converted.cause },
          outcome,
          converted.partialResult,
        );
      }
    }

    throw converted;
  } finally {
    call.close();
  }
}
