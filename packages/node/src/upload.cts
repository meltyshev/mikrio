import { setImmediate as yieldTurn } from "node:timers/promises";
import { isAnyArrayBuffer, isUint8Array } from "node:util/types";
import { Call } from "./calls.cjs";
import {
  beforeMutation,
  canceled,
  storageError,
  StorageError,
} from "./errors.cjs";
import {
  loadNative,
  type NativePutInput,
  type NativeCancellation,
  type NativeRequest,
  type NativeStore,
  type NativeUpload,
} from "./native.cjs";
import type { CallOptions } from "./index.cjs";
import type { PutObjectOutput } from "./values.cjs";

const CHUNK_BYTES = 64 * 1024;

// The built-in prototype owns the accessors for actual typed-array slots.
const typedArrayPrototype = Object.getPrototypeOf(
  Uint8Array.prototype,
) as object;

interface TransferState {
  hasSettled: boolean;
  producerFailed: boolean;
  producerCause: unknown;
  transferCause: unknown;
  primary: unknown;
  cleanup: unknown[];
}

export async function put(
  native: NativeStore,
  input: NativePutInput,
  source: Uint8Array | AsyncIterable<Uint8Array>,
  options: CallOptions | undefined,
): Promise<PutObjectOutput> {
  return transfer(
    (upload, cancellation) => native.putObject(input, upload, cancellation),
    source,
    options,
  );
}

export async function transfer<T extends object>(
  submit: (
    upload: NativeUpload,
    cancellation: NativeCancellation,
  ) => Promise<T>,
  source: Uint8Array | AsyncIterable<Uint8Array>,
  options: CallOptions | undefined,
): Promise<T> {
  const call = new Call(options, true);

  const state: TransferState = {
    hasSettled: false,
    producerFailed: false,
    producerCause: undefined,
    transferCause: undefined,
    primary: undefined,
    cleanup: [],
  };

  try {
    let upload: NativeUpload;
    let pending: Promise<T>;

    try {
      upload = new (loadNative().NativeUpload)();
    } catch (error: unknown) {
      throw beforeMutation(error);
    }

    try {
      pending = submit(upload, call.cancellation);
    } catch (error: unknown) {
      state.primary = beforeMutation(error);
      releaseProducer(upload, state);
      throw state.primary;
    }

    // Observe both owners immediately. A foreign next()/return() may never
    // settle; native cancellation must still return without awaiting that code.
    const pumping = pump(upload, source, state).catch((cause: unknown) => {
      if (state.hasSettled) {
        attachCleanup(state, cause);
        return;
      }

      state.transferCause = cause;
      call.cancellation.cancel();
    });

    try {
      return Object.freeze(await pending);
    } catch (error: unknown) {
      let converted = storageError(error);

      const outcome =
        converted instanceof StorageError
          ? converted.mutationOutcome
          : "Unknown";

      if (state.producerFailed) {
        converted = new StorageError(
          "BodyReadFailed",
          "Body producer failed",
          { cause: state.producerCause },
          outcome,
        );
      } else if (state.transferCause !== undefined) {
        const transfer = storageError(state.transferCause);

        converted =
          transfer instanceof StorageError
            ? new StorageError(
                transfer.code,
                transfer.message,
                { cause: transfer.cause },
                outcome,
              )
            : transfer;
      } else {
        const wasCanceled =
          converted instanceof StorageError &&
          converted.code === "OperationCanceled";

        if (wasCanceled && call.signal?.aborted) {
          converted = canceled(call.signal, outcome);
        }
      }

      state.primary = converted;

      if (converted instanceof Error && state.cleanup.length > 0) {
        converted.cause = new AggregateError(
          [converted.cause, ...state.cleanup],
          "Upload cleanup also failed",
        );
      }

      throw converted;
    } finally {
      state.hasSettled = true;

      // The handler owns any late rejection without holding this call open.
      // The native producer permit stays with the pump until it really settles.
      void pumping;
    }
  } finally {
    call.close();
  }
}

function attachCleanup(state: TransferState, cleanup: unknown): void {
  state.cleanup.push(cleanup);

  if (state.primary instanceof Error) {
    state.primary.cause = new AggregateError(
      [state.primary.cause, cleanup],
      "Upload cleanup also failed",
    );
  }
}

function releaseProducer(upload: NativeUpload, state: TransferState): void {
  try {
    upload.release();
  } catch (cleanup: unknown) {
    attachCleanup(state, cleanup);
  }
}

async function pump(
  upload: NativeUpload,
  source: Uint8Array | AsyncIterable<Uint8Array>,
  state: TransferState,
): Promise<void> {
  let iterator: AsyncIterator<Uint8Array> | undefined;
  let chunk: Uint8Array | undefined;
  let offset = 0;
  let producerFinished = false;
  let request: NativeRequest | undefined;
  let isProducerCall = true;

  try {
    if (isUint8Array(source)) {
      chunk = byteView(source);
    } else {
      iterator = source[Symbol.asyncIterator]();
    }

    isProducerCall = false;

    while (!state.hasSettled) {
      request = (await upload.next()) ?? undefined;

      if (request === undefined || state.hasSettled) {
        return;
      }

      while (chunk === undefined || offset === chunk.byteLength) {
        if (state.hasSettled) {
          return;
        }

        if (iterator === undefined) {
          request.finish();
          request = undefined;
          producerFinished = true;
          return;
        }

        isProducerCall = true;
        const next: unknown = await iterator.next();

        if (state.hasSettled) {
          return;
        }

        if (typeof next !== "object" || next === null) {
          throw new TypeError("body iterator must return an object");
        }

        const hasInvalidDone =
          "done" in next &&
          next.done !== undefined &&
          typeof next.done !== "boolean";

        if (hasInvalidDone) {
          throw new TypeError("body iterator done must be boolean");
        }

        if ("done" in next && next.done === true) {
          isProducerCall = false;
          request.finish();
          request = undefined;
          producerFinished = true;
          return;
        }

        if (!("value" in next) || !isUint8Array(next.value)) {
          throw new TypeError("body chunks must be Uint8Array");
        }

        chunk = byteView(next.value);
        offset = 0;
        isProducerCall = false;

        if (chunk.byteLength === 0) {
          await yieldTurn();
        }
      }

      if (state.hasSettled) {
        return;
      }

      const end = Math.min(offset + CHUNK_BYTES, chunk.byteLength);

      // The fresh non-shared buffer also makes SharedArrayBuffer-backed input
      // safe to lend to Rust; no concurrently writable foreign memory is borrowed.
      isProducerCall = true;
      const bytes = new Uint8Array(chunk.subarray(offset, end));
      isProducerCall = false;
      request.write(bytes);
      request = undefined;
      offset = end;
    }
  } catch (cause: unknown) {
    if (state.hasSettled) {
      attachCleanup(state, cause);
      return;
    }

    if (!isProducerCall) {
      const error = storageError(cause);

      if (error instanceof StorageError && error.code === "BodyClosed") {
        return;
      }

      throw cause;
    }

    state.producerFailed = true;
    state.producerCause = cause;

    if (request === undefined) {
      throw cause;
    }

    request.fail();
    request = undefined;
  } finally {
    if (request !== undefined) {
      try {
        request.fail();
      } catch (failure: unknown) {
        attachCleanup(state, failure);
      }
    }

    if (!producerFinished && iterator !== undefined) {
      try {
        // Property lookup is foreign code too. Read it once inside the same
        // cleanup boundary as invocation so a getter cannot strand admission.
        await iterator.return?.();
      } catch (cleanup: unknown) {
        attachCleanup(state, cleanup);
      }
    }

    // A pending foreign next()/return() still owns its permit. Once it settles,
    // release explicitly instead of waiting for the wrapper's eventual GC.
    releaseProducer(upload, state);
  }
}

function byteView(source: Uint8Array): Uint8Array {
  // Read internal slots through the built-in getters, ignoring own properties
  // and subclass overrides. This view allocates no payload and has no caller
  // subarray/species/iterator hooks; the pump still copies only bounded slices.
  const buffer: unknown = Reflect.get(typedArrayPrototype, "buffer", source);

  const offset: unknown = Reflect.get(
    typedArrayPrototype,
    "byteOffset",
    source,
  );

  const length: unknown = Reflect.get(
    typedArrayPrototype,
    "byteLength",
    source,
  );

  const hasValidSlots =
    isAnyArrayBuffer(buffer) &&
    typeof offset === "number" &&
    typeof length === "number";

  if (!hasValidSlots) {
    throw new TypeError("body chunk must have a valid typed-array view");
  }

  return new Uint8Array(buffer, offset, length);
}
