import assert from "node:assert/strict";
import { spawn, spawnSync, type SpawnSyncReturns } from "node:child_process";
import { getEventListeners, once } from "node:events";
import {
  mkdtemp,
  mkdir,
  readFile,
  readdir,
  realpath,
  writeFile,
} from "node:fs/promises";
import { isAbsolute, join } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";
import { test, type TestContext } from "node:test";
import { Store, StorageError } from "mikrio";

const GUARD_HELPER_PATH = fileURLToPath(
  new URL("./native_guard.py", import.meta.url),
);

const cleanups = new WeakMap<TestContext, (() => void | Promise<void>)[]>();

function cleanup(
  testContext: TestContext,
  action: () => void | Promise<void>,
): void {
  const actions = cleanups.get(testContext);

  assert.ok(
    actions,
    "a fixture must own cleanup before resources are acquired",
  );

  actions.push(action);
}

async function directory(testContext: TestContext): Promise<string> {
  const root = process.env.mikrio_test_directory;

  assert.ok(
    root,
    "qualification must supply a process-owned fixture directory",
  );

  // Canceled opens can outlive their JS promises. The parent harness removes
  // fixture files after process exit releases the native handles.
  const path = await mkdtemp(join(root, "lifecycle-"));

  const actions: (() => void | Promise<void>)[] = [];

  cleanups.set(testContext, actions);

  testContext.after(async () => {
    const failures: unknown[] = [];

    for (const action of actions.reverse()) {
      try {
        await action();
      } catch (error: unknown) {
        failures.push(error);
      }
    }

    cleanups.delete(testContext);

    if (failures.length > 0) {
      throw new AggregateError(failures, "Fixture cleanup failed");
    }
  });

  return path;
}

async function exclusiveGuard(
  testContext: TestContext,
  path: string,
): Promise<() => Promise<void>> {
  const python = process.env.mikrio_test_python;

  assert.ok(
    python,
    "qualification must supply the reviewed Python lock helper",
  );

  const child = spawn(
    python,
    ["-u", GUARD_HELPER_PATH, "hold", join(path, ".mikrio.lock")],
    { stdio: ["pipe", "pipe", "inherit"] },
  );

  const exited = once(child, "exit");

  cleanup(testContext, async () => {
    if (child.exitCode === null) {
      child.kill();
    }

    await exited;
  });

  const ready: unknown = (
    await Promise.race([
      once(child.stdout, "data"),
      exited.then(() => {
        throw new Error("Guard helper exited before readiness");
      }),
    ])
  )[0];

  assert.ok(Buffer.isBuffer(ready));
  assert.equal(ready.toString().trim(), "held");

  return async () => {
    child.stdin.end("release\n");
    assert.deepEqual(await exited, [0, null]);
  };
}

function code(expected: string): (error: unknown) => boolean {
  return (error: unknown): boolean => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, expected);
    assert.equal(error.mutationOutcome, undefined);
    assert.equal(error.partialResult, undefined);
    return true;
  };
}

async function deadline<T>(
  work: Promise<T>,
  timeoutMs: number,
  phase: string,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;

  const expired = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(() => {
      reject(new Error(`${phase} exceeded ${timeoutMs} ms`));
    }, timeoutMs);
  });

  try {
    return await Promise.race([work, expired]);
  } finally {
    clearTimeout(timer);
  }
}

function barrier(): { promise: Promise<void>; resolve: () => void } {
  let resolve: (() => void) | undefined;

  const promise = new Promise<void>((ready) => {
    resolve = ready;
  });

  assert.ok(resolve);
  return { promise, resolve };
}

async function phase<T>(
  testContext: TestContext,
  name: string,
  work: Promise<T>,
  limitMs: number,
): Promise<T> {
  const started = performance.now();

  try {
    return await deadline(work, limitMs, name);
  } finally {
    testContext.diagnostic(
      `${name}: ${Math.round(performance.now() - started)} ms`,
    );
  }
}

await test("independent connections keep immutable identity through close and reopen", async (testContext) => {
  const path = await directory(testContext);
  const first = await Store.open({ directory: path, region: "eu-west-1" });

  cleanup(testContext, async () => {
    await first.close();
  });

  const second = await Store.open({ directory: path });

  cleanup(testContext, async () => {
    await second.close();
  });

  assert.deepEqual(first.info, second.info);
  assert.match(first.info.storeId, /^[a-f0-9]{32}$/);
  assert.equal(first.info.formatVersion, 1);
  assert.equal(first.info.region, "eu-west-1");
  assert.ok(isAbsolute(first.info.directory));
  assert.equal(await realpath(first.info.directory), await realpath(path));
  assert.ok(Object.isFrozen(first.info));
  assert.equal(Reflect.set(first.info, "region", "changed"), false);
  const identity = first.info;

  await Promise.all(
    Array.from({ length: 80 }, async () => {
      await first.close();
    }),
  );

  assert.deepEqual(first.info, identity);
  const reopened = await Store.open({ directory: path });

  cleanup(testContext, async () => {
    await reopened.close();
  });

  assert.deepEqual(reopened.info, identity);
  await second.close();
  await reopened[Symbol.asyncDispose]();
});

await test("existing-only opens refuse uninitialized paths and preserve identity", async (testContext) => {
  const root = await directory(testContext);
  const missing = join(root, "missing", "store");
  const empty = join(root, "empty");
  await mkdir(empty);
  let creationReads = 0;

  await assert.rejects(
    Store.open({
      directory: missing,
      get createIfMissing() {
        creationReads += 1;
        return false;
      },
    }),
    { code: "InvalidStoreDirectory" },
  );

  assert.equal(creationReads, 1);

  for (const path of [missing, empty]) {
    await assert.rejects(
      Store.open({ directory: path, createIfMissing: false }),
      {
        code: "InvalidStoreDirectory",
      },
    );
  }

  assert.deepEqual(await readdir(root), ["empty"]);
  assert.deepEqual(await readdir(empty), []);
  const first = await Store.open({ directory: empty });

  cleanup(testContext, async () => {
    await first.close();
  });

  const peer = await Store.open({ directory: empty, createIfMissing: false });

  cleanup(testContext, async () => {
    await peer.close();
  });

  assert.deepEqual(peer.info, first.info);
  await peer.close();
  await first.close();

  const reopened = await Store.open({
    directory: empty,
    createIfMissing: false,
  });

  cleanup(testContext, async () => {
    await reopened.close();
  });

  assert.deepEqual(reopened.info, first.info);

  for (const value of [null, 0, 1, "false"]) {
    await assert.rejects(
      // @ts-expect-error Exercise invalid runtime values at the public boundary.
      Store.open({ directory: missing, createIfMissing: value }),
      TypeError,
    );
  }

  assert.deepEqual(await readdir(root), ["empty"]);
});

await test("independently loaded native images preserve each other's guards", async (testContext) => {
  const path = await directory(testContext);
  const python = process.env.mikrio_test_python;

  assert.ok(
    python,
    "qualification must supply the reviewed Python lock helper",
  );

  for (const order of ["forward", "reverse"]) {
    const result: SpawnSyncReturns<string> = spawnSync(
      process.execPath,
      [
        "--input-type=commonjs",
        "-e",
        String.raw`
          const assert = require("node:assert/strict");
          const { spawnSync } = require("node:child_process");
          const { cpSync, statSync } = require("node:fs");
          const { createRequire } = require("node:module");
          const { dirname, join } = require("node:path");

          const packageRoot = dirname(dirname(require.resolve("mikrio")));

          const replicaRoot = join(
            process.argv[1],
            "replica-" + process.argv[3],
          );

          cpSync(packageRoot, replicaRoot, { recursive: true });

          const originalRequire = createRequire(
            join(packageRoot, "package.json"),
          );

          const candidates = Object.keys(
            originalRequire("./package.json").optionalDependencies,
          ).filter((name) => {
            try {
              originalRequire.resolve(name);
              return true;
            } catch (error) {
              if (error.code === "MODULE_NOT_FOUND") {
                return false;
              }

              throw error;
            }
          });

          assert.equal(candidates.length, 1);
          const nativeName = candidates[0];

          cpSync(
            dirname(originalRequire.resolve(nativeName)),
            join(replicaRoot, "node_modules", nativeName),
            { recursive: true },
          );

          const roots = [packageRoot, replicaRoot];

          if (process.argv[3] === "reverse") {
            roots.reverse();
          }

          const [original, replica] = roots.map((root) => require(root));
          assert.notEqual(original.Store, replica.Store);
          const directory = join(process.argv[1], "store-" + process.argv[3]);

          function nativeIdentity(path) {
            const metadata = statSync(path, { bigint: true });
            return String(metadata.dev) + ":" + String(metadata.ino);
          }

          function checkGuard(expected) {
            const probe = spawnSync(
              process.argv[2],
              [
                process.argv[4],
                "probe",
                join(directory, ".mikrio.lock"),
              ],
              { timeout: 5000, encoding: "utf8" },
            );

            assert.equal(probe.error, undefined);
            assert.equal(probe.status, expected, probe.stderr);
          }

          (async () => {
            const first = await original.Store.open({ directory });
            let second;
            let held;
            let acknowledged;

            try {
              second = await replica.Store.open({ directory });
              assert.deepEqual(first.info, second.info);
              const loaded = process.report.getReport().sharedObjects;

              const nativeImages = loaded.filter(
                (path) => path.endsWith("mikrio.node"),
              ).map(nativeIdentity);

              assert.equal(new Set(nativeImages).size, 2);

              for (const root of [packageRoot, replicaRoot]) {
                assert.ok(
                  nativeImages.includes(
                    nativeIdentity(
                      createRequire(join(root, "package.json")).resolve(
                        nativeName,
                      ),
                    ),
                  ),
                );
              }

              assert.equal(
                loaded.filter((path) =>
                  /[/\\](?:lib)?mikrio_sqlite3\.(?:so|dylib|dll)$/.test(path),
                ).length,
                1,
              );

              await first.createBucket({ bucket: "bucket" });

              await first.putObject({
                bucket: "bucket",
                key: "held",
                body: Buffer.from("detached"),
              });

              const selected = await first.getObject({
                bucket: "bucket",
                key: "held",
              });

              held = selected.body;

              await second.deleteObject({
                bucket: "bucket",
                key: "held",
              });

              await second.close();

              await first.putObject({
                bucket: "bucket",
                key: "acknowledged",
                body: Buffer.from("survives"),
              });

              assert.equal(
                (
                  await first.headObject({
                    bucket: "bucket",
                    key: "acknowledged",
                  })
                ).contentLength,
                8n,
              );

              second = await replica.Store.open({ directory });

              const output = await second.getObject({
                bucket: "bucket",
                key: "acknowledged",
              });

              acknowledged = output.body;

              assert.equal(
                Buffer.from(
                  await acknowledged.readAll({ maxBytes: 8 }),
                ).toString(),
                "survives",
              );

              await acknowledged.close();

              assert.equal(
                Buffer.from(await held.readAll({ maxBytes: 8 })).toString(),
                "detached",
              );

              await held.close();
              checkGuard(1);
              await first.close();
              checkGuard(1);
              const reopened = await original.Store.open({ directory });

              try {
                assert.deepEqual(reopened.info, second.info);
              } finally {
                await reopened.close();
              }

              checkGuard(1);
              await second.close();
              assert.equal(selected.contentLength, 8n);
              checkGuard(0);
            } finally {
              await acknowledged?.close();
              await held?.close();
              await first.close();
              await second?.close();
            }
          })().catch((error) => {
            console.error(error);
            process.exitCode = 1;
          });
        `,
        path,
        python,
        order,
        GUARD_HELPER_PATH,
      ],
      { timeout: 45_000, encoding: "utf8" },
    );

    assert.equal(result.error, undefined);
    assert.equal(result.status, 0, result.stderr);
  }
});

await test("unrelated directories and conflicting persisted regions are preserved", async (testContext) => {
  const path = await directory(testContext);
  const unrelated = join(path, "unrelated");
  await mkdir(unrelated);
  await writeFile(join(unrelated, "mine"), "unchanged");

  await assert.rejects(
    Store.open({ directory: unrelated }),
    code("InvalidStoreDirectory"),
  );

  assert.deepEqual(await readdir(unrelated), ["mine"]);
  assert.equal(await readFile(join(unrelated, "mine"), "utf8"), "unchanged");

  const store = await Store.open({
    directory: join(path, "managed"),
    region: "eu-west-1",
  });

  cleanup(testContext, async () => {
    await store.close();
  });

  await assert.rejects(
    Store.open({ directory: store.info.directory, region: "us-east-1" }),
    code("RegionMismatch"),
  );
});

await test("domain validation preserves Unicode and typed local causes", async (testContext) => {
  const path = await directory(testContext);

  await assert.rejects(
    Store.open({ directory: "\ud800" }),
    code("InvalidArgument"),
  );

  await assert.rejects(
    Store.open({ directory: path, region: "\ud800" }),
    code("InvalidArgument"),
  );

  await assert.rejects(Store.open({ directory: "" }), code("InvalidArgument"));
  const filePath = join(path, "file");
  await writeFile(filePath, "owned");

  await assert.rejects(
    Store.open({ directory: join(filePath, "child") }),
    (error: unknown) => {
      code("IoError")(error);
      assert.ok(error instanceof StorageError && error.cause instanceof Error);
      return true;
    },
  );

  const unicode = await Store.open({ directory: join(path, "e\u0301-猫") });

  cleanup(testContext, async () => {
    await unicode.close();
  });

  assert.ok(unicode.info.directory.endsWith("e\u0301-猫"));
});

await test("misuse rejects promises and explicit undefined remains accepted", async (testContext) => {
  const path = await directory(testContext);

  // These deliberately bypass declarations to verify the ordinary JS boundary.
  const cases: unknown[] = [
    null,
    undefined,
    [],
    { directory: null },
    { directory: 4 },
    { directory: path, region: null },
    { directory: path, unknown: true },
  ];

  for (const input of cases) {
    const promise: unknown = Reflect.apply(Store.open.bind(Store), Store, [
      input,
    ]);

    assert.ok(promise instanceof Promise);
    await assert.rejects(promise, TypeError);
  }

  for (const control of [
    null,
    { signal: null },
    { signal: {} },
    { extra: true },
  ]) {
    const promise: unknown = Reflect.apply(Store.open.bind(Store), Store, [
      { directory: path },
      control,
    ]);

    assert.ok(promise instanceof Promise);
    await assert.rejects(promise, TypeError);
  }

  const store = await Store.open(
    { directory: path, region: undefined },
    { signal: undefined },
  );

  cleanup(testContext, async () => {
    await store.close();
  });

  assert.throws(() => structuredClone(store), { name: "DataCloneError" });
  assert.throws(() => Reflect.construct(Store, []), TypeError);
});

await test("pre-aborted acquisition leaves no directory and preserves its cause", async (testContext) => {
  const parent = await directory(testContext);
  const signal = AbortSignal.abort(new Error("caller reason"));

  await assert.rejects(
    Store.open({ directory: join(parent, "absent") }, { signal }),
    (error: unknown) => {
      code("OperationCanceled")(error);
      assert.ok(error instanceof StorageError);
      assert.equal(error.cause, signal.reason);
      return true;
    },
  );

  assert.deepEqual(await readdir(parent), []);
  assert.equal(getEventListeners(signal, "abort").length, 0);
});

await test("acquisition signal is removed after success and semantic failure", async (testContext) => {
  const path = await directory(testContext);
  const controller = new AbortController();

  const store = await Store.open(
    { directory: path },
    { signal: controller.signal },
  );

  cleanup(testContext, async () => {
    await store.close();
  });

  assert.equal(getEventListeners(controller.signal, "abort").length, 0);
  controller.abort();
  const another = await Store.open({ directory: path });

  cleanup(testContext, async () => {
    await another.close();
  });

  assert.deepEqual(store.info, another.info);
  const failed = new AbortController();

  await assert.rejects(
    Store.open(
      { directory: path, region: "eu-west-1" },
      { signal: failed.signal },
    ),
    code("RegionMismatch"),
  );

  assert.equal(getEventListeners(failed.signal, "abort").length, 0);
});

await test(
  "blocked acquisition yields the event loop and cancels before the contention budget",
  { timeout: 75_000 },
  async (testContext) => {
    const path = await directory(testContext);

    const initial = await phase(
      testContext,
      "Prepare cancellation fixture",
      Store.open({ directory: path }),
      30_000,
    );

    cleanup(testContext, async () => {
      await initial.close();
    });

    const identity = initial.info;

    await phase(
      testContext,
      "Close cancellation fixture",
      initial.close(),
      2_000,
    );

    const release = await phase(
      testContext,
      "Acquire cancellation guard",
      exclusiveGuard(testContext, path),
      2_000,
    );

    const controller = new AbortController();
    const started = performance.now();

    const waiting = Store.open(
      { directory: path },
      { signal: controller.signal },
    );

    const rejection = assert.rejects(waiting, code("OperationCanceled"));
    await delay(50); // The timeout drives JS cancellation while native guard acquisition is blocked.
    controller.abort();
    await deadline(rejection, 2_000, "Cancel blocked acquisition");
    assert.ok(performance.now() - started < 2_000);
    assert.equal(getEventListeners(controller.signal, "abort").length, 0);
    await phase(testContext, "Release cancellation guard", release(), 2_000);

    const reopened = await phase(
      testContext,
      "Reopen cancellation fixture",
      Store.open({ directory: path }),
      30_000,
    );

    cleanup(testContext, async () => {
      await reopened.close();
    });

    assert.deepEqual(reopened.info, identity);
  },
);

await test("ESM and CommonJS share classes and loading is offline and lazy", () => {
  const result = spawnSync(
    process.execPath,
    [
      "--input-type=commonjs",
      "-e",
      `
        const assert = require("node:assert/strict");
        const fs = require("node:fs");
        const net = require("node:net");
        const childProcess = require("node:child_process");

        const fail = () => {
          throw new Error("unexpected import side effect");
        };

        process.dlopen = fail;
        fs.mkdirSync = fail;
        net.Server.prototype.listen = fail;
        childProcess.spawn = fail;
        const library = require("mikrio");

        import("mikrio")
          .then((esm) => {
            assert.equal(esm.Store, library.Store);
            assert.equal(esm.StorageError, library.StorageError);
          })
          .catch((error) => {
            console.error(error);
            process.exitCode = 1;
          });
      `,
    ],
    { encoding: "utf8", timeout: 5_000 },
  );

  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
});

await test("idle native connection does not pin process exit", async (testContext) => {
  const path = await directory(testContext);
  const started = performance.now();

  const child = spawn(
    process.execPath,
    [
      "--input-type=commonjs",
      "-e",
      `
        require("mikrio")
          .Store.open({ directory: process.argv[1] })
          .then((store) => {
            globalThis.retainedStore = store;
            console.log(store.info.storeId);
          })
          .catch((error) => {
            console.error(error);
            process.exitCode = 1;
          });
      `,
      path,
    ],
    { stdio: ["ignore", "pipe", "pipe"] },
  );

  const exited = once(child, "close");
  const lines = createInterface({ input: child.stdout });
  let stderr = "";

  child.stderr.setEncoding("utf8").on("data", (chunk: string) => {
    stderr += chunk;
  });

  cleanup(testContext, async () => {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGKILL");
    }

    await exited;
    lines.close();
  });

  const ready = once(lines, "line");

  const identity: unknown = (
    await deadline(
      Promise.race([
        ready,
        exited.then(() => {
          throw new Error(`Child exited before readiness: ${stderr}`);
        }),
      ]),
      30_000,
      "Native acquisition",
    )
  )[0];

  assert.equal(typeof identity, "string");
  assert.match(String(identity), /^[a-f0-9]{32}$/);
  const acquired = performance.now();

  testContext.diagnostic(
    `Child acquisition: ${Math.round(acquired - started)} ms`,
  );

  assert.deepEqual(
    await deadline(exited, 2_000, "Idle process exit"),
    [0, null],
    stderr,
  );

  testContext.diagnostic(
    `Idle process exit: ${Math.round(performance.now() - acquired)} ms`,
  );

  const reopened = await Store.open({ directory: path });

  cleanup(testContext, async () => {
    await reopened.close();
  });

  assert.equal(reopened.info.storeId, identity);
});

await test(
  "terminating an independent Node environment preserves another connection",
  { timeout: 60_000 },
  async (testContext) => {
    const path = await directory(testContext);

    const store = await phase(
      testContext,
      "Prepare independent store",
      Store.open({ directory: path }),
      30_000,
    );

    cleanup(testContext, async () => {
      await store.close();
    });

    const worker = new Worker(
      `
        const { parentPort, workerData } = require("node:worker_threads");

        require("mikrio")
          .Store.open({ directory: workerData })
          .then((store) => {
            globalThis.retainedStore = store;
            parentPort.on("message", () => {});
            parentPort.postMessage(store.info);
          })
          .catch((error) => {
            throw error;
          });
      `,
      { eval: true, workerData: path },
    );

    cleanup(testContext, async () => {
      await worker.terminate();
    });

    const info: unknown = (
      await phase(
        testContext,
        "Worker readiness",
        once(worker, "message"),
        5_000,
      )
    )[0];

    assert.deepEqual(info, store.info);

    await phase(
      testContext,
      "Independent environment termination",
      worker.terminate(),
      2_000,
    );

    const another = await phase(
      testContext,
      "Reopen independent store",
      Store.open({ directory: path }),
      30_000,
    );

    cleanup(testContext, async () => {
      await another.close();
    });

    assert.deepEqual(another.info, store.info);
    await store.close();
    await another.close();
  },
);

await test(
  "saturated acquisition keeps explicit close available",
  { timeout: 90_000 },
  async (testContext) => {
    const path = await directory(testContext);

    const live = await phase(
      testContext,
      "Prepare saturation live store",
      Store.open({ directory: join(path, "live") }),
      30_000,
    );

    cleanup(testContext, async () => {
      await live.close();
    });

    const blockedPath = join(path, "blocked");

    const initial = await phase(
      testContext,
      "Prepare saturation blocked store",
      Store.open({ directory: blockedPath }),
      30_000,
    );

    cleanup(testContext, async () => {
      await initial.close();
    });

    await phase(
      testContext,
      "Close saturation fixture",
      initial.close(),
      2_000,
    );

    const release = await phase(
      testContext,
      "Acquire saturation guard",
      exclusiveGuard(testContext, blockedPath),
      5_000,
    );

    const controllers = Array.from({ length: 40 }, () => new AbortController());

    const abort = (): void => {
      for (const controller of controllers) {
        controller.abort();
      }
    };

    const waiting = controllers.map((controller) =>
      Store.open({ directory: blockedPath }, { signal: controller.signal }),
    );

    const settled = Promise.allSettled(waiting);

    cleanup(testContext, async () => {
      abort();
      await settled;
    });

    try {
      await phase(
        testContext,
        "Close during saturated admission",
        live.close(),
        2_000,
      );
    } finally {
      abort();
    }

    const results = await phase(
      testContext,
      "Settle canceled acquisitions",
      settled,
      5_000,
    );

    let busy = 0;

    for (const result of results) {
      if (result.status !== "rejected") {
        await result.value.close();
        assert.fail("blocked acquisition succeeded");
      }

      const error: unknown = result.reason;
      assert.ok(error instanceof StorageError);

      if (error.code === "StoreBusy") {
        busy += 1;
      } else {
        assert.equal(error.code, "OperationCanceled");
      }
    }

    assert.equal(busy, 8);

    for (const controller of controllers) {
      assert.equal(getEventListeners(controller.signal, "abort").length, 0);
    }

    await phase(testContext, "Release saturation guard", release(), 2_000);
  },
);

await test(
  "pending acquisition survives environment teardown without a late host callback",
  { timeout: 120_000 },
  async (testContext) => {
    const path = await directory(testContext);

    const store = await phase(
      testContext,
      "Prepare teardown live store",
      Store.open({ directory: join(path, "live") }),
      30_000,
    );

    cleanup(testContext, async () => {
      await store.close();
    });

    const blocked = join(path, "blocked");

    const initial = await phase(
      testContext,
      "Prepare teardown blocked store",
      Store.open({ directory: blocked }),
      30_000,
    );

    cleanup(testContext, async () => {
      await initial.close();
    });

    const identity = initial.info;
    await phase(testContext, "Close teardown fixture", initial.close(), 2_000);

    const release = await phase(
      testContext,
      "Acquire teardown guard",
      exclusiveGuard(testContext, blocked),
      5_000,
    );

    const worker = new Worker(
      `
        const { parentPort, workerData } = require("node:worker_threads");
        const { Store } = require("mikrio");
        const pending = Store.open({ directory: workerData });

        pending
          .then((store) => store.close())
          .catch((error) => {
            parentPort.postMessage({ unexpectedResult: error.code });
          });

        parentPort.postMessage("submitted");
      `,
      { eval: true, workerData: blocked },
    );

    cleanup(testContext, async () => {
      await worker.terminate();
    });

    const submitted: unknown = (
      await phase(
        testContext,
        "Pending worker readiness",
        once(worker, "message"),
        5_000,
      )
    )[0];

    assert.equal(submitted, "submitted");

    await phase(
      testContext,
      "Pending environment termination",
      worker.terminate(),
      2_000,
    );

    await phase(testContext, "Close teardown live store", store.close(), 2_000);
    await phase(testContext, "Release teardown guard", release(), 2_000);

    const reopened = await phase(
      testContext,
      "Reopen after teardown",
      Store.open({ directory: blocked }),
      30_000,
    );

    cleanup(testContext, async () => {
      await reopened.close();
    });

    assert.deepEqual(reopened.info, identity);
  },
);

await test(
  "admitted upload and body reads progress during saturated acquisition",
  { timeout: 120_000 },
  async (testContext) => {
    const path = await directory(testContext);

    const store = await phase(
      testContext,
      "Prepare transfer store",
      Store.open({ directory: join(path, "live") }),
      30_000,
    );

    cleanup(testContext, async () => {
      await store.close();
    });

    await store.createBucket({ bucket: "transfers" });

    await store.putObject({
      bucket: "transfers",
      key: "selected",
      body: Buffer.from("selected"),
    });

    const selected = await store.getObject({
      bucket: "transfers",
      key: "selected",
    });

    cleanup(testContext, async () => {
      await selected.body.close();
    });

    const blockedPath = join(path, "blocked");

    const initial = await phase(
      testContext,
      "Prepare transfer admission guard",
      Store.open({ directory: blockedPath }),
      30_000,
    );

    cleanup(testContext, async () => {
      await initial.close();
    });

    await initial.close();

    const release = await phase(
      testContext,
      "Hold transfer admission guard",
      exclusiveGuard(testContext, blockedPath),
      5_000,
    );

    const entered = barrier();
    const resume = barrier();
    const cancellation = new AbortController();

    const uploading = store.putObject(
      {
        bucket: "transfers",
        key: "uploaded",
        contentLength: 9n,
        body: (async function* () {
          entered.resolve();
          await resume.promise;
          yield Buffer.from("stream");
          yield Buffer.from("ing");
        })(),
      },
      { signal: cancellation.signal },
    );

    const uploadSettled = Promise.allSettled([uploading]);

    cleanup(testContext, async () => {
      cancellation.abort();
      resume.resolve();
      await uploadSettled;
    });

    await phase(testContext, "Observe admitted source", entered.promise, 5_000);

    const full = barrier();
    const controllers = Array.from({ length: 40 }, () => new AbortController());

    const abort = (): void => {
      for (const controller of controllers) {
        controller.abort();
      }
    };

    const waiting = controllers.map((controller) =>
      Store.open(
        { directory: blockedPath },
        { signal: controller.signal },
      ).catch((error: unknown) => {
        if (error instanceof StorageError && error.code === "StoreBusy") {
          full.resolve();
        }

        throw error;
      }),
    );

    const settled = Promise.allSettled(waiting);

    cleanup(testContext, async () => {
      abort();
      await settled;
    });

    try {
      await phase(
        testContext,
        "Observe exhausted ordinary admission",
        full.promise,
        5_000,
      );

      const prefix = await phase(
        testContext,
        "Read with ordinary admission full",
        selected.body.readChunk({ maxBytes: 3 }),
        5_000,
      );

      assert.deepEqual(prefix, Buffer.from("sel"));

      assert.deepEqual(
        await selected.body.readAll({ maxBytes: 5 }),
        Buffer.from("ected"),
      );

      resume.resolve();

      await phase(
        testContext,
        "Finish upload with ordinary admission full",
        uploading,
        15_000,
      );
    } finally {
      abort();
      cancellation.abort();
      resume.resolve();
    }

    const results = await phase(
      testContext,
      "Settle transfer admission waiters",
      settled,
      5_000,
    );

    for (const result of results) {
      if (result.status !== "rejected") {
        await result.value.close();
        assert.fail("guarded acquisition unexpectedly succeeded");
      }

      const error: unknown = result.reason;
      assert.ok(error instanceof StorageError);

      assert.ok(
        error.code === "StoreBusy" || error.code === "OperationCanceled",
      );

      assert.equal(error.mutationOutcome, undefined);
    }

    for (const controller of controllers) {
      assert.equal(getEventListeners(controller.signal, "abort").length, 0);
    }

    await phase(
      testContext,
      "Release transfer admission guard",
      release(),
      5_000,
    );

    const stored = await store.getObject({
      bucket: "transfers",
      key: "uploaded",
    });

    await using body = stored.body;

    assert.deepEqual(
      await body.readAll({ maxBytes: 9 }),
      Buffer.from("streaming"),
    );
  },
);
