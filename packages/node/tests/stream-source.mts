// Native stream fixture consumer.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { setImmediate as yieldTurn } from "node:timers/promises";

export interface Reservation {
  write(bytes: Uint8Array): void;
}

export interface StreamProbe {
  reserve(): Promise<Reservation>;
  readChunk(maxBytes: number): Promise<Uint8Array | null>;
  readAll(maxBytes: number): Promise<Uint8Array>;
  end(): void;
  failProducer(): void;
  close(): Promise<void>;
  readonly allocations: { retainedBytes: number; peakBytes: number };
  readonly isReading: boolean;
}

interface ProbeModule {
  StreamProbe: new (expected?: bigint) => StreamProbe;
  pendingCalls(): number;
}

export const addonPath = process.env.mikrio_stream_probe;
assert.ok(addonPath, "explicit instrumented addon path is required");
const loaded: unknown = createRequire(import.meta.url)(addonPath);
assert.ok(typeof loaded === "object" && loaded !== null);
assert.ok("StreamProbe" in loaded && typeof loaded.StreamProbe === "function");

assert.ok(
  "pendingCalls" in loaded && typeof loaded.pendingCalls === "function",
);

export const native = loaded as ProbeModule;
export const CHUNK_BYTES = 64 * 1024;

export class ProducerFailure extends Error {
  readonly code = "BodyReadFailed";
}

/** Pull one source chunk at a time, drain it, then permit producer reuse. */
export async function produce(
  stream: StreamProbe,
  source: AsyncIterable<Uint8Array>,
  finalizations: Promise<unknown>[],
  signal?: AbortSignal,
): Promise<void> {
  const iterator = source[Symbol.asyncIterator]();

  let removeAbort = (): void => {};

  const aborted = new Promise<never>((_resolve, reject) => {
    const cancel = (): void => {
      void stream.close().then(
        () => {
          // AbortSignal reasons are arbitrary caller-owned values.
          // eslint-disable-next-line @typescript-eslint/prefer-promise-reject-errors
          reject(signal?.reason);
        },
        (error: unknown) => {
          reject(
            new AggregateError([signal?.reason, error], "Abort cleanup failed"),
          );
        },
      );
    };

    signal?.addEventListener("abort", cancel, { once: true });

    removeAbort = () => {
      signal?.removeEventListener("abort", cancel);
    };
  });

  let finished = false;

  try {
    signal?.throwIfAborted();

    for (;;) {
      // Race only the foreign callback. Its late settlement cannot write bytes.
      const next = await Promise.race([iterator.next(), aborted]);
      signal?.throwIfAborted();

      if (next.done) {
        stream.end();
        finished = true;
        return;
      }

      const chunk = next.value;

      const isUnsharedChunk =
        chunk instanceof Uint8Array &&
        !(chunk.buffer instanceof SharedArrayBuffer);

      if (!isUnsharedChunk) {
        throw new TypeError("producer must supply an unshared Uint8Array");
      }

      for (let offset = 0; offset < chunk.byteLength; offset += CHUNK_BYTES) {
        const reservation = await stream.reserve();
        signal?.throwIfAborted();
        reservation.write(chunk.subarray(offset, offset + CHUNK_BYTES));
      }

      // Empty sources must yield to timers, cancellation and other host tasks.
      await yieldTurn();
      signal?.throwIfAborted();
    }
  } catch (cause: unknown) {
    if (signal?.aborted) {
      await stream.close();
      throw signal.reason;
    }

    stream.failProducer();
    throw new ProducerFailure("Producer failed", { cause });
  } finally {
    removeAbort();

    if (!finished) {
      // Finalization may itself stall. Observe its failure without making
      // native close depend on an arbitrary foreign callback's cooperation.
      const finalization = iterator.return?.();

      if (finalization) {
        // The fixture owner retains this separately observable outcome. Do not
        // wait here: an iterator's return may queue behind a stalled next call.
        finalizations.push(
          finalization.then(
            () => ({ ok: true }),
            (error: unknown) => ({ ok: false, error }),
          ),
        );
      }
    }
  }
}
