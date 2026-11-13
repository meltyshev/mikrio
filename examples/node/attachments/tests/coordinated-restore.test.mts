import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { copyFile, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { Store } from "@mikrio/node";
import { Attachments } from "../src/attachments.mjs";
import { loadConfig } from "../src/config.mjs";
import { configureLocalPublication, initializeLocal } from "../src/local.mjs";
import { UPLOAD_LIFETIME_MS } from "../src/model.mjs";
import { State } from "../src/state.mjs";
import { S3Storage } from "../src/storage.mjs";
import { copyOffline, inspectInventory, verifyCopy } from "./offline-copy.mjs";
import { BYTES, INPUT, OWNER } from "./support.mjs";

await test("complete offline app and store backups restore in isolated processes and mismatched recovery points keep reconciliation paused", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "mikrio-coordinated-restore-"));
  const resources = new AsyncDisposableStack();

  t.after(async () => {
    await resources.disposeAsync();

    if (t.passed) {
      await rm(root, { recursive: true });
    } else {
      t.diagnostic(`retained coordinated restore: ${root}`);
    }
  });

  const source = join(root, "application");
  const path = await initializeLocal(source);
  const config = await loadConfig(path);
  assert.ok(config.local);

  const openStore = async (): Promise<Store> => {
    assert.ok(config.local);
    const store = await Store.open({ directory: config.local.directory });
    resources.defer(() => store.close());
    return store;
  };

  const openState = async (): Promise<State> => {
    const state = await State.open(config.state);
    resources.defer(() => state.close());
    return state;
  };

  const initialStore = await openStore();

  const backend = (await initialStore.listPrincipals({})).principals.find(
    (principal) => principal.name === "attachment-backend",
  );

  assert.ok(backend);
  await initialStore.close();
  await configureLocalPublication(config, backend.principalId);
  config.publicAttachments = true;

  await writeFile(path, JSON.stringify(config, null, 2) + "\n", {
    mode: 0o600,
  });

  const initialState = await openState();

  await initialState.call("addAccount", {
    accountId: OWNER,
    username: OWNER,
    salt: "00".repeat(16),
    passwordHash: "00".repeat(32),
  });

  await initialState.close();
  const earlier = join(root, "earlier");
  const earlierManifest = await copyOffline(source, earlier);
  assert.equal(earlierManifest["app.sqlite3-wal"], undefined);

  const store = await openStore();
  const state = await openState();
  const listener = await store.listen({ host: "127.0.0.1", port: 0 });
  resources.defer(() => listener.close());

  const storage = new S3Storage({
    ...config.storage,
    endpoint: listener.endpoint,
  });

  resources.defer(() => storage.close());
  const { bucket, prefix } = config.state;
  const app = new Attachments(state, storage, bucket, prefix, true);

  const upload = async () => {
    const issued = await app.create(OWNER, INPUT);

    const response = await fetch(issued.upload.url, {
      method: "PUT",
      headers: issued.upload.headers,
      body: BYTES,
      signal: AbortSignal.timeout(10_000),
    });

    await response.arrayBuffer();
    assert.equal(response.status, 200);
    return issued.attemptId;
  };

  const attached = await upload();
  await app.finalize(OWNER, attached);

  // Retain a remote public tag with current private app intent, simulating a
  // delayed publication. Restore must inspect and reconcile both authorities.
  await store.putObjectTagging({
    bucket,
    key: prefix + attached,
    tags: [{ key: "visibility", value: "public" }],
  });

  const abandoned = await upload();

  const claimed = await state.call("claimCleanup", {
    now: Date.now() + UPLOAD_LIFETIME_MS,
  });

  assert.deepEqual(
    claimed.map((attempt) => attempt.attemptId),
    [abandoned],
  );

  const pending = await upload();

  const expected = {
    storeId: store.info.storeId,
    attached,
    pending,
    abandoned,
  };

  storage.close();
  await listener.close();
  await state.close();
  await store.close();

  // No live opener or app worker remains. The fixture owns the whole source
  // for both complete copies; the manifest lives outside managed storage.
  const backup = join(root, "backup");
  const manifest = await copyOffline(source, backup);
  assert.ok(manifest["store/store.json"]);
  assert.ok(manifest["store/metadata.sqlite3"]);
  assert.ok(manifest["app.sqlite3"]);
  assert.ok(manifest["config.json"]);
  assert.equal(manifest["app.sqlite3-wal"], undefined);

  await writeFile(
    join(root, "backup-manifest.json"),
    JSON.stringify(manifest, null, 2),
    { mode: 0o600 },
  );

  const expectedPath = join(root, "expected.json");
  await writeFile(expectedPath, JSON.stringify(expected), { mode: 0o600 });
  await verifyCopy(source, backup, manifest);

  // A later revocation is absent from the backup. Its restored key must be
  // rotated before listening, without rewriting this now-stopped live store.
  const live = await openStore();

  await live.deleteAccessKey({
    principalId: backend.principalId,
    accessKeyId: config.storage.backend.accessKeyId,
  });

  await live.close();
  const stoppedLive = await inspectInventory(source);

  for (const mode of ["matched", "missing-object", "lost-history"] as const) {
    const working = join(root, mode);
    await copyOffline(backup, working);

    if (mode === "missing-object") {
      await rm(join(working, "store"), { recursive: true });
      await copyOffline(join(earlier, "store"), join(working, "store"));
    }

    if (mode === "lost-history") {
      await copyFile(
        join(earlier, "app.sqlite3"),
        join(working, "app.sqlite3"),
      );
    }

    const child = await promisify(execFile)(
      process.execPath,
      [
        "--no-global-search-paths",
        fileURLToPath(new URL("./recovery-child.mjs", import.meta.url)),
        working,
        expectedPath,
        mode,
      ],
      { timeout: 60_000, killSignal: "SIGKILL", maxBuffer: 64 * 1024 },
    );

    assert.deepEqual(JSON.parse(child.stdout), {
      classification: mode,
      accepted: mode === "matched",
    });

    for (const secret of [
      config.storage.backend.secretAccessKey,
      config.storage.uploader.secretAccessKey,
    ]) {
      assert.equal((child.stdout + child.stderr).includes(secret), false);
    }

    assert.deepEqual(await inspectInventory(source), stoppedLive);
    assert.deepEqual(await inspectInventory(backup), manifest);
    assert.deepEqual(await inspectInventory(earlier), earlierManifest);
  }
});
