import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { ConsoleDelivery, loadProbe } from "./diagnostic-delivery.mjs";

const addonPath = process.env.mikrio_diagnostics_probe;
assert.ok(addonPath, "explicit instrumented addon path is required");
const SECRET = "secret-key signed-url private-path body-canary";

async function until(condition: () => boolean): Promise<void> {
  const limit = performance.now() + 5000;

  while (!condition()) {
    assert.ok(performance.now() < limit, "diagnostic handshake timed out");
    await delay(5);
  }
}

await test("idle host delivery redacts error details and coalesces repeated failures", async (testContext) => {
  const original = console.error;

  testContext.after(() => {
    console.error = original;
  });

  const lines: unknown[][] = [];

  console.error = (...values: unknown[]) => {
    lines.push(values);
  };

  const probe = loadProbe(addonPath);
  testContext.after(() => probe.close());
  const delivery = new ConsoleDelivery(probe);

  testContext.after(() => {
    delivery.close();
  });

  for (let count = 0; count < 10_000; count++) {
    probe.emit(SECRET);
  }

  assert.equal(probe.stats.queued, 1);
  assert.equal(probe.stats.coalesced, 9999n);
  await until(() => lines.length === 1);
  const line: unknown = lines[0]?.[0];
  assert.equal(typeof line, "string");

  assert.match(
    String(line),
    /work=cleanup failure=permission occurrences=10000/,
  );

  for (const canary of SECRET.split(" ")) {
    assert.ok(!String(line).includes(canary));
  }

  assert.equal(probe.stats.queued, 0);
});

await test("throwing and rejecting host output cannot escape or alter native state", async (testContext) => {
  const original = console.error;

  testContext.after(() => {
    console.error = original;
  });

  let calls = 0;

  // Deliberately test a nonstandard host replacement; the adapter must contain it.
  // eslint-disable-next-line @typescript-eslint/no-misused-promises
  console.error = () => {
    calls++;

    if (calls === 1) {
      throw new Error(SECRET);
    }

    return Promise.reject(new Error(SECRET));
  };

  const probe = loadProbe(addonPath);
  testContext.after(() => probe.close());
  const delivery = new ConsoleDelivery(probe);

  testContext.after(() => {
    delivery.close();
  });

  probe.emit(SECRET);
  await until(() => probe.stats.observerFailures === 1n);
  probe.emit(SECRET);
  await until(() => probe.stats.observerFailures === 2n);
  assert.equal(calls, 2);
  delivery.close();
  assert.equal(probe.stats.queued, 0);
});

await test("a stalled observer retains one event and close discards queued work without waiting", async (testContext) => {
  const original = console.error;

  testContext.after(() => {
    console.error = original;
  });

  let calls = 0;
  let release: ((reason: Error) => void) | undefined;

  const stalled = new Promise<void>((_resolve, reject) => {
    release = reject;
  });

  // Deliberately test a Promise-returning replacement of the host's void sink.
  // eslint-disable-next-line @typescript-eslint/no-misused-promises
  console.error = () => {
    calls++;
    return stalled;
  };

  const probe = loadProbe(addonPath);
  testContext.after(() => probe.close());
  const delivery = new ConsoleDelivery(probe);

  testContext.after(() => {
    delivery.close();
    release?.(new Error(SECRET));
  });

  probe.emit(SECRET);
  await until(() => calls === 1);

  for (let count = 0; count < 10_000; count++) {
    probe.emit(SECRET);
  }

  await until(() => delivery.pollCount >= 3);
  assert.equal(calls, 1);
  assert.equal(probe.stats.queued, 1);
  delivery.close();
  assert.equal(probe.stats.queued, 0);
  assert.equal(probe.stats.dropped, 10_000n);
  probe.emit(SECRET);
  release?.(new Error(SECRET));
  await until(() => probe.stats.observerFailures === 1n);
  assert.equal(calls, 1);
  assert.equal(probe.stats.dropped, 10_001n);
});

await test("host output can reenter and close the native queue without a lock cycle", async (testContext) => {
  const original = console.error;

  testContext.after(() => {
    console.error = original;
  });

  const probe = loadProbe(addonPath);
  testContext.after(() => probe.close());
  let calls = 0;

  console.error = () => {
    calls++;
    probe.emit(SECRET);
    delivery.close();
  };

  const delivery = new ConsoleDelivery(probe);

  testContext.after(() => {
    delivery.close();
  });

  probe.emit(SECRET);
  await until(() => calls === 1);
  assert.equal(probe.stats.queued, 0);
  assert.equal(probe.stats.dropped, 1n);
});

await test(
  "host delivery installs no global exception handler and does not pin an idle process",
  { timeout: 10_000 },
  async (testContext) => {
    const child = spawn(
      process.execPath,
      [
        "--input-type=module",
        "-e",
        `
          import {
            ConsoleDelivery,
            loadProbe,
          } from ${JSON.stringify(new URL("./diagnostic-delivery.mjs", import.meta.url).href)};

          const before = process.rawListeners("uncaughtException");

          globalThis.retainedDelivery = new ConsoleDelivery(
            loadProbe(process.env.mikrio_diagnostics_probe),
          );

          const hasGlobalHandler =
            before.length !== process.rawListeners("uncaughtException").length;

          if (hasGlobalHandler) {
            throw Error("global handler installed");
          }

          console.log("ready");
        `,
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );

    const exited = once(child, "close");

    testContext.after(async () => {
      if (child.exitCode === null) {
        child.kill("SIGKILL");
      }

      await exited;
    });

    let stdout = "";
    let stderr = "";

    child.stdout.setEncoding("utf8").on("data", (chunk: string) => {
      stdout += chunk;
    });

    child.stderr.setEncoding("utf8").on("data", (chunk: string) => {
      stderr += chunk;
    });

    assert.deepEqual(await exited, [0, null], stderr);
    assert.equal(stdout.trim(), "ready");
  },
);
