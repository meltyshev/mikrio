import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { Store } from "mikrio";
import { Auth } from "../src/auth.mjs";
import { loadConfig } from "../src/config.mjs";
import { configureLocalPublication, initializeLocal } from "../src/local.mjs";
import { AppError } from "../src/model.mjs";
import { State } from "../src/state.mjs";

await test("installed CLI provisions private storage and an app account without printing credentials or replacing existing state", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "mikrio-attachment-cli-"));

  t.after(async () => {
    if (t.passed) {
      await rm(root, { recursive: true });
    } else {
      t.diagnostic(`retained failed CLI state: ${root}`);
    }
  });

  const directory = join(root, "application");
  const main = fileURLToPath(new URL("../src/main.mjs", import.meta.url));

  const invoke = (arguments_: string[], input = "") =>
    new Promise<{ code: number | null; output: string }>((resolve, reject) => {
      const child = spawn(process.execPath, [main, ...arguments_], {
        stdio: ["pipe", "pipe", "pipe"],
      });

      let output = "";
      const timeout = setTimeout(() => child.kill("SIGKILL"), 30_000);

      child.stdout.setEncoding("utf8").on("data", (chunk: string) => {
        output += chunk;
      });

      child.stderr.setEncoding("utf8").on("data", (chunk: string) => {
        output += chunk;
      });

      child.once("error", (error) => {
        clearTimeout(timeout);
        reject(error);
      });

      child.once("close", (code) => {
        clearTimeout(timeout);
        resolve({ code, output });
      });

      child.stdin.end(input);
    });

  const initialized = await invoke(["local-init", directory]);
  assert.equal(initialized.code, 0, initialized.output);
  const path = join(directory, "config.json");
  const config = await loadConfig(path);
  assert.equal((await stat(path)).mode & 0o077, 0);
  const previous = await readFile(path);
  const repeated = await invoke(["local-init", directory]);
  assert.equal(repeated.code, 1);
  assert.deepEqual(await readFile(path), previous);
  const password = "private CLI password canary";

  const account = await invoke(
    ["account", path, "local-user"],
    password + "\n",
  );

  assert.equal(account.code, 0, account.output);
  const output = initialized.output + repeated.output + account.output;

  for (const secret of [
    config.storage.backend.secretAccessKey,
    config.storage.uploader.secretAccessKey,
    password,
  ]) {
    assert.equal(output.includes(secret), false);
  }

  const state = await State.open(config.state);

  try {
    const auth = new Auth(state);
    const token = await auth.login("local-user", password);
    assert.equal((await auth.requireSession(token)).username, "local-user");
  } finally {
    await state.close();
  }
});

await test("local provisioning retains both the operation failure and a later close failure", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "mikrio-attachment-disposal-"));

  t.after(async () => {
    if (t.passed) {
      await rm(root, { recursive: true });
    } else {
      t.diagnostic(`retained failed provisioning state: ${root}`);
    }
  });

  const operationFailure = new Error("provisioning failed");
  const cleanupFailure = new Error("close failed");
  const open = Store.open.bind(Store);

  // Inject failures at the public resource boundary; close the real native store.
  t.mock.method(Store, "open", async (options: Parameters<typeof open>[0]) => {
    const store = await open(options);
    const close = store.close.bind(store);

    t.mock.method(store, "createBucket", () =>
      Promise.reject(operationFailure),
    );

    t.mock.method(store, "close", async () => {
      await close();
      throw cleanupFailure;
    });

    return store;
  });

  await assert.rejects(
    initializeLocal(join(root, "application")),
    (error: unknown) => {
      assert.ok(error instanceof SuppressedError);
      assert.equal(error.error, cleanupFailure);
      assert.equal(error.suppressed, operationFailure);
      return true;
    },
  );
});

await test("local provisioning permits the exact app preflight without granting storage authority", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "mikrio-attachment-cors-"));

  t.after(async () => {
    if (t.passed) {
      await rm(root, { recursive: true });
    } else {
      t.diagnostic(`retained failed CORS provisioning state: ${root}`);
    }
  });

  const path = await initializeLocal(join(root, "application"));
  const config = await loadConfig(path);
  assert.ok(config.local);
  await using store = await Store.open({ directory: config.local.directory });

  const configuration = await store.getBucketCors({
    bucket: config.state.bucket,
  });

  assert.equal(configuration.corsRules.length, 1);

  assert.deepEqual(configuration.corsRules[0]?.allowedOrigins, [
    config.appOrigin,
  ]);

  await using listener = await store.listen({ host: "127.0.0.1", port: 0 });
  const target = `${listener.endpoint}/${config.state.bucket}/private/example`;

  for (const [origin, method, headers, status] of [
    [
      config.appOrigin,
      "PUT",
      "content-type,content-disposition,if-none-match",
      200,
    ],
    [config.appOrigin, "GET", "", 200],
    [config.appOrigin, "HEAD", "", 200],
    ["http://127.0.0.1:3002", "PUT", "content-type", 403],
    [config.appOrigin, "DELETE", "", 403],
    [config.appOrigin, "PUT", "x-unapproved-header", 403],
  ] as const) {
    const response = await fetch(target, {
      method: "OPTIONS",
      headers: {
        origin,
        "access-control-request-method": method,
        ...(headers === ""
          ? {}
          : { "access-control-request-headers": headers }),
      },
      signal: AbortSignal.timeout(5000),
    });

    await response.arrayBuffer();
    assert.equal(response.status, status, `${origin} ${method} ${headers}`);

    if (status === 200) {
      assert.equal(
        response.headers.get("access-control-allow-origin"),
        config.appOrigin,
      );
    } else {
      assert.equal(response.headers.get("access-control-allow-origin"), null);
    }
  }

  const unsigned = await fetch(target, {
    headers: { origin: config.appOrigin },
    signal: AbortSignal.timeout(5000),
  });

  await unsigned.arrayBuffer();
  assert.equal(unsigned.status, 403);
});

await test("local publication verifies the backend and preserves unrelated policy before opt-in", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "mikrio-attachment-public-"));

  t.after(async () => {
    if (t.passed) {
      await rm(root, { recursive: true });
    } else {
      t.diagnostic(`retained failed publication state: ${root}`);
    }
  });

  const config = await loadConfig(await initializeLocal(join(root, "app")));
  assert.ok(config.local);
  const bucket = config.state.bucket;
  await using store = await Store.open({ directory: config.local.directory });
  const principals = (await store.listPrincipals({})).principals;

  const backend = principals.find(
    (entry) => entry.name === "attachment-backend",
  );

  const uploader = principals.find(
    (entry) => entry.name === "attachment-uploader",
  );

  assert.ok(backend && uploader);
  const original = await store.getPublicAccessBlock({ bucket });

  await assert.rejects(
    configureLocalPublication(config, uploader.principalId),
    (error: unknown) => error instanceof AppError && error.code === "invalid",
  );

  assert.deepEqual(await store.getPublicAccessBlock({ bucket }), original);

  const unrelated = JSON.stringify({
    Version: "2012-10-17",
    Statement: {
      Effect: "Deny",
      Principal: "*",
      Action: "s3:DeleteObject",
      Resource: `arn:aws:s3:::${bucket}/*`,
    },
  });

  await store.putBucketPolicy({ bucket, document: unrelated });

  await assert.rejects(
    configureLocalPublication(config, backend.principalId),
    (error: unknown) => error instanceof AppError && error.code === "conflict",
  );

  assert.equal((await store.getBucketPolicy({ bucket })).document, unrelated);
  assert.deepEqual(await store.getPublicAccessBlock({ bucket }), original);
  await store.deleteBucketPolicy({ bucket });
  await configureLocalPublication(config, backend.principalId);
  const granted = await store.getBucketPolicy({ bucket });
  await configureLocalPublication(config, backend.principalId);
  assert.deepEqual(await store.getBucketPolicy({ bucket }), granted);

  assert.deepEqual(
    (await store.getPublicAccessBlock({ bucket })).configuration,
    {
      blockPublicAcls: true,
      ignorePublicAcls: true,
      blockPublicPolicy: false,
      restrictPublicBuckets: false,
    },
  );
});
