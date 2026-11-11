import assert from "node:assert/strict";
import {
  copyFile,
  mkdtemp,
  rename,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { Store } from "mikrio";
import { Attachments } from "../src/attachments.mjs";
import { loadConfig } from "../src/config.mjs";
import { initializeLocal } from "../src/local.mjs";
import { UPLOAD_LIFETIME_MS } from "../src/model.mjs";
import { State } from "../src/state.mjs";
import { S3Storage } from "../src/storage.mjs";
import { BYTES, INPUT, OWNER, expectAppError } from "./support.mjs";

await test("unaccepted stale, missing, corrupt or mismatched app recovery points preserve real attached bytes until authoritative state is restored", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-attachment-restore-"));
  const resources = new AsyncDisposableStack();

  t.after(async () => {
    await resources.disposeAsync();

    if (t.passed) {
      await rm(directory, { recursive: true });
    } else {
      t.diagnostic(`retained app restore state: ${directory}`);
    }
  });

  const config = await loadConfig(
    await initializeLocal(join(directory, "application")),
  );

  assert.ok(config.local);
  const store = await Store.open({ directory: config.local.directory });
  resources.defer(() => store.close());
  const listener = await store.listen({ host: "127.0.0.1", port: 0 });
  resources.defer(() => listener.close());

  const storage = new S3Storage({
    ...config.storage,
    endpoint: listener.endpoint,
  });

  resources.defer(() => storage.close());

  const open = async (): Promise<State> => {
    const state = await State.open(config.state);
    resources.defer(() => state.close());
    return state;
  };

  const empty = await open();

  await empty.call("addAccount", {
    accountId: OWNER,
    username: "restore-owner",
    salt: "00".repeat(16),
    passwordHash: "00".repeat(32),
  });

  await empty.close();

  // Every backup/restore happens after the sole app worker has closed SQLite.
  // No engine SQL or engine data files are copied or opened here.
  const snapshot = async (name: string): Promise<string> => {
    await assert.rejects(stat(config.state.path + "-wal"), { code: "ENOENT" });
    const path = join(directory, name + ".sqlite3");
    await copyFile(config.state.path, path);

    assert.notEqual(
      (await stat(path)).ino,
      (await stat(config.state.path)).ino,
    );

    return path;
  };

  const noHistory = await snapshot("before-attempt");
  const pending = await open();

  const issuer = new Attachments(
    pending,
    storage,
    config.state.bucket,
    config.state.prefix,
    true,
  );

  const issued = await issuer.create(OWNER, INPUT);
  await pending.close();
  const stalePending = await snapshot("before-attachment");
  const current = await open();

  const app = new Attachments(
    current,
    storage,
    config.state.bucket,
    config.state.prefix,
    true,
  );

  const uploaded = await fetch(issued.upload.url, {
    method: "PUT",
    headers: issued.upload.headers,
    body: BYTES,
    signal: AbortSignal.timeout(10_000),
  });

  await uploaded.arrayBuffer();
  assert.equal(uploaded.status, 200);
  const attached = await app.finalize(OWNER, issued.attemptId);
  await current.close();
  const authoritative = await snapshot("attached-authority");

  const assertStoredBytes = async (): Promise<void> => {
    const selected = await store.getObject({
      bucket: config.state.bucket,
      key: config.state.prefix + issued.attemptId,
    });

    try {
      assert.deepEqual(
        Buffer.from(await selected.body.readAll({ maxBytes: BYTES.length })),
        BYTES,
      );
    } finally {
      await selected.body.close();
    }
  };

  for (const backup of [noHistory, stalePending]) {
    await copyFile(backup, config.state.path);
    const restored = await open();

    const paused = new Attachments(
      restored,
      storage,
      config.state.bucket,
      config.state.prefix,
      false,
    );

    assert.deepEqual(
      await restored.call("attachments", { accountId: OWNER }),
      [],
    );

    await assert.rejects(
      paused.cleanup(Date.now() + UPLOAD_LIFETIME_MS),
      expectAppError("unavailable"),
    );

    await assertStoredBytes();
    await restored.close();
  }

  const missing = join(directory, "missing-preserved.sqlite3");
  await rename(config.state.path, missing);
  await assert.rejects(State.open(config.state), expectAppError("unavailable"));
  await assert.rejects(stat(config.state.path), { code: "ENOENT" });
  await assertStoredBytes();
  await rename(missing, config.state.path);

  for (const mismatch of [
    { storageId: "mismatched-dataset" },
    { bucket: "other-bucket" },
    { prefix: "other-prefix/" },
  ]) {
    await assert.rejects(
      State.open({ ...config.state, ...mismatch }),
      expectAppError("unavailable"),
    );

    await assertStoredBytes();
  }

  await writeFile(config.state.path, Buffer.alloc(4096, 0xa5));
  await assert.rejects(State.open(config.state), expectAppError("unavailable"));
  await assertStoredBytes();
  await copyFile(authoritative, config.state.path);
  const corrupt = new DatabaseSync(config.state.path);

  try {
    // This satisfies the row CHECK constraints but contradicts the durable
    // attachment reference. Startup must reject the inconsistent history.
    corrupt
      .prepare(
        "UPDATE attempts SET status = 'abandoned', next_cleanup_at = 0 WHERE attempt_id = ?",
      )
      .run(issued.attemptId);
  } finally {
    corrupt.close();
  }

  await assert.rejects(State.open(config.state), expectAppError("unavailable"));
  await assertStoredBytes();

  await copyFile(authoritative, config.state.path);
  const reconciled = await open();

  const accepted = new Attachments(
    reconciled,
    storage,
    config.state.bucket,
    config.state.prefix,
    true,
  );

  assert.deepEqual(await reconciled.call("attachments", { accountId: OWNER }), [
    attached,
  ]);

  assert.deepEqual(await accepted.cleanup(Date.now() + UPLOAD_LIFETIME_MS), {
    deleted: 0,
    failed: 0,
  });

  const downloaded = await fetch(
    await accepted.download(OWNER, issued.attemptId),
    { signal: AbortSignal.timeout(10_000) },
  );

  assert.equal(downloaded.status, 200);
  assert.deepEqual(Buffer.from(await downloaded.arrayBuffer()), BYTES);
  await reconciled.close();
  await assert.rejects(accepted.cleanup(), expectAppError("unavailable"));
  await assertStoredBytes();
});
