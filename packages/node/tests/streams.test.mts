import assert from "node:assert/strict";
import { getEventListeners, once } from "node:events";
import { setImmediate as yieldTurn } from "node:timers/promises";
import { test, type TestContext } from "node:test";
import { Worker } from "node:worker_threads";
import {
  addonPath,
  CHUNK_BYTES,
  native,
  produce,
  ProducerFailure,
  type StreamProbe,
} from "./stream-source.mjs";

function probe(testContext: TestContext, expected?: bigint): StreamProbe {
  const stream = new native.StreamProbe(expected);

  testContext.after(async () => {
    await stream.close();
  });

  return stream;
}

function code(expected: string): (error: unknown) => boolean {
  return (error: unknown): boolean => {
    assert.ok(error instanceof Error && "code" in error);
    assert.equal(error.code, expected);
    return true;
  };
}

async function until(condition: () => boolean): Promise<void> {
  const end = performance.now() + 5000;

  while (!condition()) {
    assert.ok(performance.now() < end, "native handshake timed out");
    await yieldTurn();
  }
}

async function consume(stream: StreamProbe): Promise<Map<number, number>> {
  const counts = new Map<number, number>();

  for (;;) {
    const chunk = await stream.readChunk(8191);

    if (chunk === null) {
      return counts;
    }

    assert.ok(chunk.length > 0 && chunk.length <= 8191);

    for (const byte of chunk) {
      counts.set(byte, (counts.get(byte) ?? 0) + 1);
    }
  }
}

await test("empty input is not EOF and explicit close is terminal", async (testContext) => {
  const stream = probe(testContext, 2n);
  (await stream.reserve()).write(new Uint8Array());
  const pending = stream.readChunk(1);
  await until(() => stream.isReading);
  await assert.rejects(stream.readChunk(1), code("BodyInUse"));
  const mutable = Uint8Array.of(17, 29);
  (await stream.reserve()).write(mutable);
  mutable.fill(0);
  assert.deepEqual([...((await pending) ?? [])], [17]);
  stream.end();
  assert.deepEqual([...(await stream.readAll(1))], [29]);
  assert.equal(await stream.readChunk(1), null);
  assert.equal(await stream.readChunk(1), null);
  assert.deepEqual([...(await stream.readAll(0))], []);
  await stream.close();
  await stream.close();
  await assert.rejects(stream.readChunk(1), code("BodyClosed"));
  assert.equal(stream.allocations.retainedBytes, 0);
});

await test("a partial read keeps the whole allocation charged under backpressure", async (testContext) => {
  const stream = probe(testContext);
  (await stream.reserve()).write(new Uint8Array(CHUNK_BYTES).fill(11));
  (await stream.reserve()).write(new Uint8Array(CHUNK_BYTES).fill(12));
  let admitted = false;

  const third = stream.reserve().then((reservation) => {
    admitted = true;
    return reservation;
  });

  await until(() => native.pendingCalls() === 1);
  assert.deepEqual([...((await stream.readChunk(1)) ?? [])], [11]);
  await yieldTurn();
  assert.equal(admitted, false);

  assert.deepEqual(stream.allocations, {
    retainedBytes: 131072,
    peakBytes: 131072,
  });

  assert.equal((await stream.readChunk(CHUNK_BYTES))?.length, CHUNK_BYTES - 1);
  (await third).write(Uint8Array.of(13));
  stream.end();

  assert.deepEqual(
    await consume(stream),
    new Map([
      [12, CHUNK_BYTES],
      [13, 1],
    ]),
  );

  assert.deepEqual(stream.allocations, { retainedBytes: 0, peakBytes: 131072 });
});

await test("large and reused producer buffers drain within two native chunks", async (testContext) => {
  const stream = probe(testContext, BigInt(8 * 1024 * 1024 + 2 * CHUNK_BYTES));
  const finalizations: Promise<unknown>[] = [];
  let finalized = false;

  async function* source(): AsyncGenerator<Uint8Array> {
    try {
      await yieldTurn();
      const huge = new Uint8Array(8 * 1024 * 1024).fill(42);
      yield huge;
      huge.fill(0);
      const reused = new Uint8Array(CHUNK_BYTES).fill(7);
      yield reused;
      reused.fill(9);
      yield reused;
      reused.fill(0);
    } finally {
      finalized = true;
    }
  }

  const producing = produce(stream, source(), finalizations);
  const counts = await consume(stream);
  await producing;

  assert.deepEqual(
    counts,
    new Map([
      [42, 8 * 1024 * 1024],
      [7, CHUNK_BYTES],
      [9, CHUNK_BYTES],
    ]),
  );

  assert.equal(finalized, true);
  assert.deepEqual(finalizations, []);
  assert.ok(stream.allocations.peakBytes <= 131072);
  assert.equal(stream.allocations.retainedBytes, 0);

  testContext.diagnostic(
    JSON.stringify({ bytes: 8519680, ...stream.allocations }),
  );
});

await test("producer errors preserve their cause and cannot become successful EOF", async (testContext) => {
  const stream = probe(testContext);
  const cause = new Error("controlled producer failure");
  const finalizations: Promise<unknown>[] = [];

  async function* source(): AsyncGenerator<Uint8Array> {
    await yieldTurn();
    yield Uint8Array.of(1);
    throw cause;
  }

  const result = assert.rejects(
    produce(stream, source(), finalizations),
    (error: unknown) => {
      assert.ok(error instanceof ProducerFailure);
      assert.equal(error.cause, cause);
      return true;
    },
  );

  await result;
  await assert.rejects(stream.readChunk(1), code("BodyReadFailed"));
  await assert.rejects(stream.readChunk(1), code("BodyClosed"));
  assert.deepEqual(await Promise.all(finalizations), [{ ok: true }]);
});

await test("length mismatch and bounded readAll fail without retaining queued bytes", async (testContext) => {
  const short = probe(testContext, 3n);
  (await short.reserve()).write(Uint8Array.of(1));

  assert.throws(() => {
    short.end();
  }, code("LengthMismatch"));

  await assert.rejects(short.readChunk(4), code("LengthMismatch"));
  const long = probe(testContext, 1n);
  const reservation = await long.reserve();

  assert.throws(() => {
    reservation.write(Uint8Array.of(1, 2));
  }, code("LengthMismatch"));

  await assert.rejects(long.readChunk(4), code("LengthMismatch"));
  const bounded = probe(testContext);
  (await bounded.reserve()).write(new Uint8Array(CHUNK_BYTES));
  (await bounded.reserve()).write(new Uint8Array(CHUNK_BYTES));
  await assert.rejects(bounded.readAll(17), code("BodyTooLarge"));
  await assert.rejects(bounded.readChunk(1), code("BodyClosed"));

  for (const stream of [short, long, bounded]) {
    assert.equal(stream.allocations.retainedBytes, 0);
  }
});

await test("invalid numeric limits and oversized slices fail before copying", async (testContext) => {
  for (const value of [-1n, 1n << 64n]) {
    assert.throws(() => new native.StreamProbe(value), code("InvalidArgument"));
  }

  const stream = probe(testContext);

  for (const value of [-1, 0.5, NaN, Infinity, 2 ** 32]) {
    assert.throws(() => {
      void stream.readChunk(value);
    }, code("InvalidArgument"));
  }

  await assert.rejects(stream.readChunk(0), code("InvalidArgument"));
  const reservation = await stream.reserve();

  assert.throws(() => {
    reservation.write(new Uint8Array(CHUNK_BYTES + 1));
  }, code("InvalidArgument"));

  assert.deepEqual(stream.allocations, { retainedBytes: 0, peakBytes: 0 });
});

await test("close settles native reads before a stalled producer returns and rejects late writes", async (testContext) => {
  const stream = probe(testContext);
  const late = await stream.reserve();
  const reading = assert.rejects(stream.readChunk(1), code("BodyClosed"));
  await until(() => stream.isReading);
  await stream.close();
  await reading;
  assert.equal(stream.isReading, false);

  assert.throws(() => {
    late.write(Uint8Array.of(1));
  }, code("BodyClosed"));

  assert.throws(() => {
    stream.end();
  }, code("BodyClosed"));

  assert.deepEqual(stream.allocations, { retainedBytes: 0, peakBytes: 0 });
});

await test("empty and stalled iterators cancel without pinning callbacks or listeners", async (testContext) => {
  for (const stall of [false, true]) {
    const stream = probe(testContext);
    const controller = new AbortController();
    const reason = new Error("controlled cancellation");
    const finalizations: Promise<unknown>[] = [];
    let pulls = 0;
    let returned = false;
    let release: (() => void) | undefined;

    const blocked = new Promise<void>((resolve) => {
      release = resolve;
    });

    async function* source(): AsyncGenerator<Uint8Array> {
      try {
        for (;;) {
          pulls++;

          if (stall) {
            await blocked;
          }

          yield new Uint8Array();
        }
      } finally {
        returned = true;
      }
    }

    const producing = assert.rejects(
      produce(stream, source(), finalizations, controller.signal),
      (error: unknown) => error === reason,
    );

    await until(() => pulls > 0);
    controller.abort(reason);
    await producing;
    assert.equal(getEventListeners(controller.signal, "abort").length, 0);
    assert.equal(stream.allocations.retainedBytes, 0);
    release?.();
    assert.deepEqual(await Promise.all(finalizations), [{ ok: true }]);
    assert.equal(returned, true);
    await assert.rejects(stream.readChunk(1), code("BodyClosed"));
  }
});

await test("worker termination releases a blocked native stream future", async (testContext) => {
  assert.equal(native.pendingCalls(), 0);

  for (let run = 0; run < 3; run++) {
    const worker = new Worker(
      `
        const { parentPort, workerData } = require("node:worker_threads");
        const { StreamProbe } = require(workerData);

        const stream = new StreamProbe();
        stream.readChunk(1).catch(() => {});
        parentPort.postMessage("reading");
        setInterval(() => {}, 1000);
      `,
      { eval: true, workerData: addonPath },
    );

    testContext.after(async () => {
      await worker.terminate();
    });

    assert.deepEqual(await once(worker, "message"), ["reading"]);
    await until(() => native.pendingCalls() === 1);
    await worker.terminate();
    await until(() => native.pendingCalls() === 0);
  }
});

await test("aborted entry avoids pulling and retains iterator finalization errors separately", async (testContext) => {
  const stream = probe(testContext);
  const controller = new AbortController();
  const reason = new Error("already canceled");
  const cleanup = new Error("controlled finalization failure");
  controller.abort(reason);
  let pulls = 0;

  const source: AsyncIterableIterator<Uint8Array> = {
    [Symbol.asyncIterator]() {
      return this;
    },
    next() {
      pulls++;
      return Promise.resolve({ done: false, value: Uint8Array.of(1) });
    },
    return() {
      return Promise.reject(cleanup);
    },
  };

  const finalizations: Promise<unknown>[] = [];

  await assert.rejects(
    produce(stream, source, finalizations, controller.signal),
    (error: unknown) => error === reason,
  );

  assert.equal(pulls, 0);

  assert.deepEqual(await Promise.all(finalizations), [
    { ok: false, error: cleanup },
  ]);

  assert.equal(getEventListeners(controller.signal, "abort").length, 0);
  await assert.rejects(stream.readChunk(1), code("BodyClosed"));
});
