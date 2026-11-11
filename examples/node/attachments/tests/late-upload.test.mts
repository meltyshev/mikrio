import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";
import { Store, StorageError } from "mikrio";
import { Attachments } from "../src/attachments.mjs";
import { loadConfig } from "../src/config.mjs";
import { initializeLocal } from "../src/local.mjs";
import { CLEANUP_RETRY_MS, type Attempt } from "../src/model.mjs";
import { State } from "../src/state.mjs";
import { S3Storage } from "../src/storage.mjs";
import { BYTES, INPUT, OWNER, expectAppError } from "./support.mjs";

await test("admitted HTTP upload commits after real URL expiry and terminal compaction, then prefix reconciliation removes it after restart", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-attachment-late-"));
  const resources = new AsyncDisposableStack();

  t.after(async () => {
    await resources.disposeAsync();

    if (t.passed) {
      await rm(directory, { recursive: true });
    } else {
      t.diagnostic(`retained late-upload state: ${directory}`);
    }
  });

  const configPath = await initializeLocal(join(directory, "application"));
  const config = await loadConfig(configPath);
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
  const state = await State.open(config.state);
  resources.defer(() => state.close());

  await state.call("addAccount", {
    accountId: OWNER,
    username: "late-owner",
    salt: "00".repeat(16),
    passwordHash: "00".repeat(32),
  });

  // The owned app-state boundary selects a short deadline. Real wall time,
  // presigning, HTTP admission and cleanup expiry all execute without a fake clock.
  const attemptId = randomUUID();
  const createdAt = Date.now();

  const attempt: Attempt = {
    ...INPUT,
    attemptId,
    accountId: OWNER,
    bucket: config.state.bucket,
    key: config.state.prefix + attemptId,
    status: "pending",
    createdAt,
    expiresAt: createdAt + 8000,
  };

  await state.call("createAttempt", attempt);
  const upload = await storage.upload(attempt);

  const app = new Attachments(
    state,
    storage,
    attempt.bucket,
    config.state.prefix,
    true,
  );

  const admitted = Promise.withResolvers<void>();
  const completed = Promise.withResolvers<number>();
  const settled = completed.promise.catch(() => 0);

  const writer = httpRequest(
    upload.url,
    {
      method: "PUT",
      agent: false,
      headers: {
        ...upload.headers,
        "content-length": String(BYTES.length),
        expect: "100-continue",
      },
    },
    (response) => {
      admitted.reject(new Error("upload completed before body admission"));

      response.once("error", () =>
        completed.reject(new Error("owned response transport failed")),
      );

      response.once("end", () => completed.resolve(response.statusCode ?? 0));
      response.resume();
    },
  );

  resources.defer(async () => {
    writer.destroy();
    await settled;
  });

  writer.once("error", () => {
    admitted.reject(new Error("owned upload transport failed"));
    completed.reject(new Error("owned upload transport failed"));
  });

  writer.setTimeout(20_000, () =>
    writer.destroy(new Error("owned upload watchdog expired")),
  );

  writer.once("continue", () => admitted.resolve());
  writer.flushHeaders();
  await admitted.promise;
  t.diagnostic("HTTP 100 Continue observed before releasing any body bytes");

  const expectNoSuchKey = (error: unknown): boolean => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, "NoSuchKey");
    return true;
  };

  await assert.rejects(
    store.headObject({ bucket: attempt.bucket, key: attempt.key }),
    expectNoSuchKey,
  );

  await delay(Math.max(0, attempt.expiresAt - Date.now() + 1000));
  assert.ok(Date.now() > attempt.expiresAt);

  const expired = await fetch(upload.url, {
    method: "PUT",
    headers: upload.headers,
    body: BYTES,
  });

  const errorXml = await expired.text();
  assert.equal(expired.status, 403);
  assert.equal(errorXml.includes("<Code>AccessDenied</Code>"), true);

  assert.equal(
    errorXml.includes("<Message>Request has expired</Message>"),
    true,
  );

  assert.deepEqual(await app.cleanup(), { deleted: 1, failed: 0 });
  const retryAfter = Date.now() + CLEANUP_RETRY_MS + 100;

  await assert.rejects(
    state.call("attempt", { accountId: OWNER, attemptId }),
    expectAppError("notFound"),
  );

  await assert.rejects(
    store.headObject({ bucket: attempt.bucket, key: attempt.key }),
    expectNoSuchKey,
  );

  t.diagnostic(
    "Fresh replay expired and app cleanup completed before the admitted body resumed",
  );

  // Advance a new traversal beyond the paused UUID key after its history was
  // compacted. These ordinary objects have no application owner and are reclaimed.
  for (let index = 0; index < 17; index += 1) {
    await store.putObject({
      bucket: attempt.bucket,
      key: config.state.prefix + `zz-scan-${index.toString().padStart(2, "0")}`,
      body: BYTES,
    });
  }

  assert.deepEqual(await app.cleanup(), { deleted: 16, failed: 0 });

  writer.end(BYTES);
  assert.equal(await settled, 200);

  const late = await store.getObject({
    bucket: attempt.bucket,
    key: attempt.key,
  });

  try {
    assert.deepEqual(
      Buffer.from(await late.body.readAll({ maxBytes: BYTES.length })),
      BYTES,
    );
  } finally {
    await late.body.close();
  }

  await assert.rejects(
    app.finalize(OWNER, attemptId),
    expectAppError("notFound"),
  );

  assert.deepEqual(await app.cleanup(), { deleted: 1, failed: 0 });

  assert.equal(
    (await store.headObject({ bucket: attempt.bucket, key: attempt.key }))
      .contentLength,
    BigInt(BYTES.length),
  );

  await state.close();
  const reopened = await State.open(config.state);
  resources.defer(() => reopened.close());

  const restarted = new Attachments(
    reopened,
    storage,
    attempt.bucket,
    config.state.prefix,
    true,
  );

  await assert.rejects(
    restarted.finalize(OWNER, attemptId),
    expectAppError("notFound"),
  );

  await delay(Math.max(0, retryAfter - Date.now()));
  assert.deepEqual(await restarted.cleanup(), { deleted: 1, failed: 0 });

  await assert.rejects(
    store.headObject({ bucket: attempt.bucket, key: attempt.key }),
    expectNoSuchKey,
  );

  assert.deepEqual(
    await reopened.call("attachments", { accountId: OWNER }),
    [],
  );

  await assert.rejects(
    reopened.call("attempt", { accountId: OWNER, attemptId }),
    expectAppError("notFound"),
  );

  t.diagnostic(
    "Restarted cleanup removed late bytes after the real retry interval; compacted terminal history was not needed for rediscovery",
  );
});
