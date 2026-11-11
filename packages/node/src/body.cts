import { markAsUncloneable } from "node:worker_threads";
import { Call } from "./calls.cjs";
import { canceled, storageError, StorageError } from "./errors.cjs";
import { byteBound } from "./inputs.cjs";
import type { CallOptions } from "./index.cjs";
import type { NativeBody } from "./native.cjs";
import type { ReadAllInput, ReadChunkInput } from "./values.cjs";

const CONSTRUCTOR_KEY = Symbol("ObjectBody constructor");
const abandonedCalls = new FinalizationRegistry<Call>((call) => call.close());
let construct: (native: NativeBody, call: Call) => ObjectBody;

/**
 * One protected cursor. Close unused bodies explicitly to release native work.
 * EOF releases read protection; explicit close makes later reads fail with BodyClosed.
 * Cancellation or I/O failure is terminal. Overlapping reads reject with BodyInUse.
 */
export class ObjectBody {
  readonly #native: NativeBody;
  readonly #sourceCall: Call;
  #closing: Promise<void> | undefined;

  static {
    construct = (native, call) => new ObjectBody(native, call, CONSTRUCTOR_KEY);
  }

  private constructor(
    native: NativeBody,
    call: Call,
    key: typeof CONSTRUCTOR_KEY,
  ) {
    if (key !== CONSTRUCTOR_KEY) {
      throw new TypeError("ObjectBody is returned by getObject");
    }

    this.#native = native;
    this.#sourceCall = call;

    const weakNative = new WeakRef(native);
    call.onCancel(() => weakNative.deref()?.cancel());
    abandonedCalls.register(this, call, this);
    markAsUncloneable(this);
  }

  /** Read at most a positive maxBytes, returning nonempty bytes or null at EOF. */
  async readChunk(
    input: ReadChunkInput,
    options?: CallOptions,
  ): Promise<Uint8Array | null> {
    const maxBytes = byteBound(input, true);

    return await this.#read(options, async (call) => {
      const bytes = await this.#native.readChunk(maxBytes, call.cancellation);

      if (bytes === null) {
        this.#release();
      }

      return bytes;
    });
  }

  /** Collect at most maxBytes; zero is valid, and BodyTooLarge closes the body. */
  async readAll(
    input: ReadAllInput,
    options?: CallOptions,
  ): Promise<Uint8Array> {
    const maxBytes = byteBound(input, false);

    return await this.#read(options, async (call) => {
      const bytes = await this.#native.readAll(maxBytes, call.cancellation);
      this.#release();

      return bytes;
    });
  }

  /** Join native cleanup. Concurrent closes share one attempt; failure can retry. */
  async close(): Promise<void> {
    this.#release();
    let attempt: Promise<void> | undefined;

    try {
      attempt = this.#closing ?? this.#native.close();
      this.#closing = attempt;
      await attempt;
    } catch (error: unknown) {
      throw storageError(error);
    } finally {
      if (this.#closing === attempt) {
        this.#closing = undefined;
      }
    }
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }

  async #read<T>(
    options: CallOptions | undefined,
    run: (call: Call) => Promise<T>,
  ): Promise<T> {
    let call: Call | undefined;

    try {
      call = new Call(options, false);
      call.onCancel(() => this.#native.cancel());

      return await run(call);
    } catch (error: unknown) {
      const converted = storageError(error);

      const hasTerminalFailure =
        converted instanceof StorageError &&
        !["InvalidArgument", "BodyInUse", "StoreBusy"].includes(converted.code);

      if (hasTerminalFailure) {
        this.#native.cancel();
        this.#release();
      }

      const wasCanceled =
        converted instanceof StorageError &&
        converted.code === "OperationCanceled";

      if (wasCanceled && call?.signal?.aborted) {
        throw canceled(call.signal);
      }

      throw converted;
    } finally {
      call?.close();
    }
  }

  #release(): void {
    this.#sourceCall.close();
    abandonedCalls.unregister(this);
  }
}

export async function fromNative(
  native: NativeBody,
  call: Call,
): Promise<ObjectBody> {
  const body = construct(native, call);

  if (call.signal?.aborted) {
    const error = canceled(call.signal);

    try {
      await body.close();
    } catch (cleanup: unknown) {
      throw new AggregateError(
        [error, storageError(cleanup)],
        "GET cancellation cleanup failed",
        { cause: cleanup },
      );
    }

    throw error;
  }

  return body;
}
