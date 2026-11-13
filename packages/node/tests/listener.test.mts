import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { createConnection, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inspect, promisify } from "node:util";
import { beforeEach, test, type TestContext } from "node:test";
import { Worker } from "node:worker_threads";
import { Store, StorageError } from "@mikrio/node";

const exec = promisify(execFile);

async function testDirectory(
  context: TestContext,
  prefix: string,
): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), prefix));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed listener state: ${directory}`);
    }
  });

  return directory;
}

function failure(code: string): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, code);
    return true;
  };
}

await test("listener: ready, isolated close, snapshots and relisten", async (context) => {
  const directory = await testDirectory(context, "mikrio-listener-");
  await using store = await Store.open({ directory });
  await using peer = await Store.open({ directory });
  await store.createBucket({ bucket: "listener" });
  await using server = await store.listen();
  await using second = await peer.listen();
  const snapshot = server.credentials;
  assert.ok(!inspect(snapshot).includes(snapshot.secretAccessKey));
  assert.ok(!JSON.stringify(snapshot).includes(snapshot.secretAccessKey));
  assert.ok(Object.isFrozen(snapshot));
  assert.notEqual(snapshot.accessKeyId, second.credentials.accessKeyId);

  assert.equal(
    (await fetch(server.endpoint + "/listener", { method: "HEAD" })).status,
    403,
  );

  await assert.rejects(store.listen(), failure("ListenerAlreadyRunning"));
  await Promise.all([server.close(), server.close()]);
  assert.equal(server.credentials.secretAccessKey, snapshot.secretAccessKey);
  await store.headBucket({ bucket: "listener" });
  await using replacement = await store.listen({ credentials: snapshot });
  await store.close();
  assert.equal(replacement.region, "us-east-1");

  await assert.rejects(
    fetch(replacement.endpoint, { signal: AbortSignal.timeout(5000) }),
    TypeError,
  );

  assert.equal(
    (await fetch(second.endpoint + "/listener", { method: "HEAD" })).status,
    403,
  );
});

await test("listener: option validation and acquisition cancellation leave the store usable", async (context) => {
  const directory = await testDirectory(context, "mikrio-listener-values-");
  await using store = await Store.open({ directory });

  for (const port of [-1, 65536, NaN, Infinity, 1.5, 2 ** 32]) {
    await assert.rejects(store.listen({ port }), failure("InvalidArgument"));
  }

  for (const host of ["localhost", "", "127.0.0.1:9000"]) {
    await assert.rejects(store.listen({ host }), failure("InvalidArgument"));
  }

  await assert.rejects(
    store.listen({ publicEndpoint: "" }),
    failure("InvalidArgument"),
  );

  // @ts-expect-error optional fields reject null at the host boundary.
  await assert.rejects(store.listen({ port: null }), TypeError);
  const controller = new AbortController();
  controller.abort(new Error("canceled acquisition"));

  await assert.rejects(
    store.listen({}, { signal: controller.signal }),
    failure("OperationCanceled"),
  );

  {
    await using server = await store.listen({
      host: "0.0.0.0",
      publicEndpoint: "https://example.invalid/",
    });

    assert.match(server.endpoint, /^http:\/\/127\.0\.0\.1:\d+$/);
  }

  await store.createBucket({ bucket: "after-listen" });
});

await test("listener: active listener retains the environment and parent close releases it", async (context) => {
  const directory = await testDirectory(context, "mikrio-listener-lifetime-");
  // Arrange durable initialization outside the child lifetime watchdog. The
  // independently loaded child still opens, listens, retains and closes itself.
  await using initialized = await Store.open({ directory });
  await initialized.close();

  try {
    const script = `
      import { Store } from "@mikrio/node";

      const store = await Store.open({ directory: process.argv[1] });
      const server = await store.listen();
      console.log("ready");

      setTimeout(() => {
        store.close().then(
          () => {
            console.log(server.region, "closed");
          },
          (error) => {
            console.error(error);
            process.exitCode = 1;
          },
        );
      }, 200).unref();
    `;

    const output = await exec(
      process.execPath,
      ["--input-type=module", "--eval", script, directory],
      { timeout: 15000 },
    );

    assert.equal(output.stdout, "ready\nus-east-1 closed\n");
    assert.equal(output.stderr, "");
  } catch (error) {
    if (error instanceof Error) {
      const output = error as Error & { stdout?: string; stderr?: string };

      context.diagnostic(
        JSON.stringify({ stdout: output.stdout, stderr: output.stderr }),
      );
    }

    throw error;
  }
});

const WORKER_TEST_NAME =
  "listener: worker termination settles accepted sockets and preserves its peer";
let workerDirectory: string | undefined;

beforeEach(
  async (context) => {
    if (context.name !== WORKER_TEST_NAME) {
      return;
    }

    assert.ok("after" in context);

    // Hook setup precedes the test's lifetime watchdog. The scenario still
    // opens its own connection and performs every worker/socket assertion.
    const started = performance.now();
    workerDirectory = await testDirectory(context, "mikrio-listener-worker-");
    await using initialized = await Store.open({ directory: workerDirectory });
    await initialized.close();

    context.diagnostic(
      `Prepare durable listener fixture: ${performance.now() - started} ms`,
    );
  },
  { timeout: 120000 },
);

await test(WORKER_TEST_NAME, { timeout: 15000 }, async () => {
  assert.ok(workerDirectory);
  const directory = workerDirectory;
  await using store = await Store.open({ directory });

  let socket: Socket | undefined = undefined;

  await using workerLifetime = {
    worker: new Worker(new URL("./persistent-worker.mjs", import.meta.url), {
      workerData: { directory, mode: "listener" },
    }),
    async [Symbol.asyncDispose]() {
      socket?.destroy();
      await this.worker.terminate();
    },
  };

  const worker = workerLifetime.worker;

  let workerFailure: Error | undefined;

  const ready = new Promise<unknown>((resolve, reject) => {
    worker.once("message", resolve);

    worker.once("error", (error: Error) => {
      workerFailure = error;
      reject(error);
    });

    worker.once("exit", (status) =>
      reject(new Error(`worker exited before listener readiness: ${status}`)),
    );
  });

  const endpoint = await ready;
  assert.ok(typeof endpoint === "string");
  await store.createBucket({ bucket: "listener" });
  await using peer = await store.listen();
  const address = new URL(endpoint);

  const accepted = createConnection({
    host: address.hostname,
    port: Number(address.port),
  });

  socket = accepted;
  let socketFailure: Error | undefined;

  accepted.on("error", (error: NodeJS.ErrnoException) => {
    if (error.code !== "ECONNRESET") {
      socketFailure = error;
    }
  });

  const closed = new Promise<void>((resolve) => {
    accepted.once("close", resolve);
  });

  accepted.setTimeout(5000, () => {
    accepted.destroy(new Error("accepted connection did not settle"));
  });

  const response = await new Promise<string>((resolve, reject) => {
    let headers = "";

    const incomplete = (): void => {
      reject(new Error("connection closed before the HEAD response"));
    };

    const receive = (chunk: Buffer): void => {
      headers += chunk.toString("latin1");

      if (headers.length > 32768) {
        reject(new Error("HEAD response exceeded the fixture bound"));
        return;
      }

      if (headers.includes("\r\n\r\n")) {
        accepted.off("data", receive);
        accepted.off("error", reject);
        accepted.off("close", incomplete);
        resolve(headers);
      }
    };

    accepted.on("data", receive);
    accepted.once("error", reject);
    accepted.once("close", incomplete);

    accepted.write(`HEAD /listener HTTP/1.1\r\nHost: ${address.host}\r\n\r\n`);
  });

  assert.match(response, /^HTTP\/1\.1 403 /);
  assert.doesNotMatch(response, /\r\nconnection: close\r\n/i);
  assert.equal(accepted.destroyed, false);
  // The response proves this connection was accepted. Leave its next request
  // incomplete so environment teardown must settle the owned connection.
  accepted.write(`HEAD /listener HTTP/1.1\r\nHost: ${address.host}\r\n`);
  assert.equal(await worker.terminate(), 1);
  await closed;
  assert.equal(workerFailure, undefined);
  assert.equal(socketFailure, undefined);

  await store.headBucket({ bucket: "listener" });

  assert.equal(
    (
      await fetch(peer.endpoint + "/listener", {
        method: "HEAD",
        signal: AbortSignal.timeout(5000),
      })
    ).status,
    403,
  );

  await peer.close();
  await using replacement = await store.listen({ port: Number(address.port) });
  assert.equal(replacement.endpoint, endpoint);
  await replacement.close();
  await store.close();
  await using reopened = await Store.open({ directory });
  await reopened.headBucket({ bucket: "listener" });
});
