import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { Attachments } from "../src/attachments.mjs";
import { CLEANUP_RETRY_MS } from "../src/model.mjs";
import { State } from "../src/state.mjs";
import { verifyText } from "../src/storage.mjs";
import {
  BYTES,
  INPUT,
  OWNER,
  OTHER,
  MemoryObjects,
  readChunks,
  expectAppError,
  createFixture,
} from "./support.mjs";

await test("durable attempts authorize owners, separate duplicate filenames and attach exactly once without a second upload", async (t) => {
  const { state, states, options } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const first = await app.create(OWNER, INPUT);
  const second = await app.create(OTHER, INPUT);
  assert.notEqual(first.attemptId, second.attemptId);

  const attempt = await state.call("attempt", {
    accountId: OWNER,
    attemptId: first.attemptId,
  });

  assert.equal(attempt.key, options.prefix + first.attemptId);
  assert.notEqual(attempt.key, options.prefix + INPUT.filename);

  await assert.rejects(
    app.finalize(OTHER, first.attemptId),
    expectAppError("notFound"),
  );

  await assert.rejects(
    app.download(OTHER, first.attemptId),
    expectAppError("notFound"),
  );

  await assert.rejects(
    app.download(OWNER, first.attemptId),
    expectAppError("notFound"),
  );

  storage.objects.set(attempt.key, BYTES);
  const attached = await app.finalize(OWNER, first.attemptId);
  assert.deepEqual(await app.finalize(OWNER, first.attemptId), attached);

  assert.deepEqual(await state.call("attachments", { accountId: OWNER }), [
    attached,
  ]);

  assert.equal(storage.uploaded.length, 2);
  assert.deepEqual(storage.deleted, []);
  await state.close();

  const reopened = await State.open(options);
  states.push(reopened);
  const changedEndpoint = new MemoryObjects();

  changedEndpoint.download = (selected) =>
    Promise.resolve("https://replacement.invalid/" + selected.key);

  const restarted = new Attachments(
    reopened,
    changedEndpoint,
    options.bucket,
    options.prefix,
    true,
  );

  assert.equal(
    await restarted.download(OWNER, first.attemptId),
    "https://replacement.invalid/" + attempt.key,
  );

  assert.deepEqual(await restarted.finalize(OWNER, first.attemptId), attached);

  await assert.rejects(
    restarted.download(OTHER, first.attemptId),
    expectAppError("notFound"),
  );
});

await test("cleanup wins during verification and prefix reconciliation deletes late bytes after compaction and a lost reply", async (t) => {
  const { state, options, states } = await createFixture(t);
  const storage = new MemoryObjects();
  const release = Promise.withResolvers<void>();
  storage.holdVerification = release.promise;

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const issued = await app.create(OWNER, INPUT);

  const attempt = await state.call("attempt", {
    accountId: OWNER,
    attemptId: issued.attemptId,
  });

  storage.objects.set(attempt.key, BYTES);
  const finalizing = app.finalize(OWNER, issued.attemptId);
  const denied = assert.rejects(finalizing, expectAppError("notFound"));

  try {
    await storage.verified.promise;
    storage.loseDeleteResponse = true;

    assert.deepEqual(await app.cleanup(issued.expiresAt), {
      deleted: 0,
      failed: 1,
    });

    assert.equal(storage.objects.has(attempt.key), false);
  } finally {
    release.resolve();
  }

  await denied;
  assert.deepEqual(await state.call("attachments", { accountId: OWNER }), []);
  await state.close();

  const reopened = await State.open(options);
  states.push(reopened);

  const restarted = new Attachments(
    reopened,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  storage.objects.set(attempt.key, BYTES);
  storage.loseDeleteResponse = false;

  await assert.rejects(
    restarted.finalize(OWNER, issued.attemptId),
    expectAppError("notFound"),
  );

  assert.deepEqual(
    await restarted.cleanup(issued.expiresAt + CLEANUP_RETRY_MS),
    { deleted: 1, failed: 0 },
  );

  assert.deepEqual(storage.deleted, [attempt.key, attempt.key]);

  await assert.rejects(
    reopened.call("attempt", {
      accountId: OWNER,
      attemptId: issued.attemptId,
    }),
    expectAppError("notFound"),
  );
});

await test("attachment wins before cleanup, while unused URLs expire without deleting a live attachment", async (t) => {
  const { state, options } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const live = await app.create(OWNER, INPUT);
  const unused = await app.create(OWNER, INPUT);
  const key = options.prefix + live.attemptId;
  storage.objects.set(key, BYTES);
  const attached = await app.finalize(OWNER, live.attemptId);

  assert.deepEqual(await app.cleanup(unused.expiresAt), {
    deleted: 1,
    failed: 0,
  });

  assert.deepEqual(storage.deleted, [options.prefix + unused.attemptId]);
  assert.deepEqual(storage.objects.get(key), BYTES);

  assert.deepEqual(await state.call("attachments", { accountId: OWNER }), [
    attached,
  ]);
});

await test("unaccepted restore and unavailable authoritative state never begin a delete", async (t) => {
  const { state, options } = await createFixture(t);
  const storage = new MemoryObjects();

  const paused = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    false,
  );

  const issued = await paused.create(OWNER, INPUT);

  await assert.rejects(
    paused.cleanup(issued.expiresAt),
    expectAppError("unavailable"),
  );

  assert.equal(
    (
      await state.call("attempt", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    ).status,
    "pending",
  );

  await state.close();

  const unavailable = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  await assert.rejects(
    unavailable.cleanup(issued.expiresAt),
    expectAppError("unavailable"),
  );

  assert.deepEqual(storage.deleted, []);

  await assert.rejects(
    State.open({ ...options, storageId: "different-dataset" }),
    expectAppError("unavailable"),
  );
});

await test("an app database outage between cleanup requests stops the batch and preserves attached and unchecked bytes", async (t) => {
  const { state, options, states } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const live = await app.create(OWNER, INPUT);
  const first = await app.create(OWNER, INPUT);
  const second = await app.create(OWNER, INPUT);

  for (const issued of [live, first, second]) {
    storage.objects.set(options.prefix + issued.attemptId, BYTES);
  }

  const attached = await app.finalize(OWNER, live.attemptId);
  const deleteObject = storage.delete.bind(storage);

  storage.delete = async (object) => {
    await deleteObject(object);
    // The first request has settled. Closing the actual worker makes the next
    // authoritative read fail before another destructive network request.
    await state.close();
  };

  const now = Math.max(first.expiresAt, second.expiresAt);
  await assert.rejects(app.cleanup(now), expectAppError("unavailable"));

  const candidates = [first, second].map(
    (issued) => options.prefix + issued.attemptId,
  );

  assert.equal(storage.deleted.length, 1);
  assert.ok(candidates.includes(storage.deleted[0] ?? ""));
  assert.equal(candidates.filter((key) => storage.objects.has(key)).length, 1);
  assert.deepEqual(storage.objects.get(options.prefix + live.attemptId), BYTES);

  storage.delete = deleteObject;
  const reopened = await State.open(options);
  states.push(reopened);

  const restarted = new Attachments(
    reopened,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  assert.deepEqual(await restarted.cleanup(now + CLEANUP_RETRY_MS), {
    deleted: 2,
    failed: 0,
  });

  assert.deepEqual(storage.deleted.slice(1).sort(), candidates.sort());

  assert.deepEqual(await reopened.call("attachments", { accountId: OWNER }), [
    attached,
  ]);

  assert.deepEqual(storage.objects.get(options.prefix + live.attemptId), BYTES);

  for (const issued of [first, second]) {
    await assert.rejects(
      restarted.finalize(OWNER, issued.attemptId),
      expectAppError("notFound"),
    );
  }
});

await test("application transaction failure retains a pending object and permits verified retry", async (t) => {
  const { state, options, states } = await createFixture(t);
  const storage = new MemoryObjects();

  const initial = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const issued = await initial.create(OWNER, INPUT);
  storage.objects.set(options.prefix + issued.attemptId, BYTES);
  await state.close();
  const injector = new DatabaseSync(options.path);

  try {
    injector.exec(
      "CREATE TRIGGER reject_attachment BEFORE UPDATE OF status ON attempts WHEN NEW.status = 'attached' BEGIN SELECT RAISE(ABORT, 'owned test failure after attachment insert'); END",
    );
  } finally {
    injector.close();
  }

  const failing = await State.open(options);
  states.push(failing);

  const app = new Attachments(
    failing,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  await assert.rejects(
    app.finalize(OWNER, issued.attemptId),
    expectAppError("unavailable"),
  );

  assert.equal(
    (
      await failing.call("attempt", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    ).status,
    "pending",
  );

  assert.deepEqual(await failing.call("attachments", { accountId: OWNER }), []);
  await failing.close();

  const repair = new DatabaseSync(options.path);

  try {
    repair.exec("DROP TRIGGER reject_attachment");
  } finally {
    repair.close();
  }

  const reopened = await State.open(options);
  states.push(reopened);

  const retry = new Attachments(
    reopened,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  assert.equal(
    (await retry.finalize(OWNER, issued.attemptId)).attachmentId,
    issued.attemptId,
  );

  assert.deepEqual(
    storage.objects.get(options.prefix + issued.attemptId),
    BYTES,
  );
});

await test("cleanup claims bounded ordered batches and rolls back expiration when retry scheduling fails", async (t) => {
  const { state, options, states } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const live = await app.create(OWNER, INPUT);
  storage.objects.set(options.prefix + live.attemptId, BYTES);
  const attached = await app.finalize(OWNER, live.attemptId);
  const pending = await app.create(OWNER, INPUT);
  const now = Date.now();

  const attemptIds = Array.from(
    { length: 18 },
    (_, index) => `cleanup-${String(index).padStart(2, "0")}`,
  );

  for (const [index, attemptId] of attemptIds.entries()) {
    await state.call("createAttempt", {
      ...INPUT,
      attemptId,
      accountId: index % 2 === 0 ? OWNER : OTHER,
      bucket: options.bucket,
      key: options.prefix + attemptId,
      status: "pending",
      createdAt: now - 2000,
      expiresAt: now - 1000 + index,
    });
  }

  await state.close();
  let db = new DatabaseSync(options.path);

  try {
    db.exec(`CREATE TRIGGER reject_cleanup_retry BEFORE UPDATE OF next_cleanup_at ON attempts
      WHEN OLD.attempt_id = 'cleanup-15' AND OLD.status = 'abandoned'
      BEGIN SELECT RAISE(ABORT, 'owned retry scheduling failure'); END`);
  } finally {
    db.close();
  }

  const failing = await State.open(options);
  states.push(failing);

  await assert.rejects(
    failing.call("claimCleanup", { now }),
    expectAppError("unavailable"),
  );

  for (const [index, attemptId] of attemptIds.entries()) {
    assert.equal(
      (
        await failing.call("attempt", {
          attemptId,
          accountId: index % 2 === 0 ? OWNER : OTHER,
        })
      ).status,
      "pending",
    );
  }

  await failing.close();
  db = new DatabaseSync(options.path);

  try {
    db.exec("DROP TRIGGER reject_cleanup_retry");
  } finally {
    db.close();
  }

  const reopened = await State.open(options);
  states.push(reopened);
  const first = await reopened.call("claimCleanup", { now });
  const second = await reopened.call("claimCleanup", { now });

  assert.deepEqual(
    first.map((attempt) => attempt.attemptId),
    attemptIds.slice(0, 16),
  );

  assert.deepEqual(
    second.map((attempt) => attempt.attemptId),
    attemptIds.slice(16),
  );

  assert.ok(
    [...first, ...second].every((attempt) => attempt.status === "abandoned"),
  );

  assert.deepEqual(await reopened.call("claimCleanup", { now }), []);

  assert.deepEqual(
    await reopened.call("claimCleanup", { now: now + CLEANUP_RETRY_MS - 1 }),
    [],
  );

  assert.deepEqual(
    (await reopened.call("claimCleanup", { now: now + CLEANUP_RETRY_MS })).map(
      (attempt) => attempt.attemptId,
    ),
    attemptIds.slice(0, 16),
  );

  assert.equal(
    (
      await reopened.call("attempt", {
        accountId: OWNER,
        attemptId: pending.attemptId,
      })
    ).status,
    "pending",
  );

  assert.deepEqual(await reopened.call("attachments", { accountId: OWNER }), [
    attached,
  ]);
});

await test("content checks reject false MIME, invalid UTF-8, wrong length and digest across chunk boundaries", async () => {
  const valid = Buffer.from("Hello, café 🌍\n");

  const describeContent = (bytes: Buffer) => ({
    contentLength: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  });

  await verifyText(readChunks(valid), describeContent(valid));

  for (const bytes of [
    Buffer.from([0xff]),
    Buffer.from([0x50, 0x4b, 0, 3]),
    Buffer.from([0xe2, 0x82]),
  ]) {
    await assert.rejects(
      verifyText(readChunks(bytes), describeContent(bytes)),
      expectAppError("invalid"),
    );
  }

  await assert.rejects(
    verifyText(readChunks(valid), {
      ...describeContent(valid),
      contentLength: valid.length + 1,
    }),
    expectAppError("invalid"),
  );

  await assert.rejects(
    verifyText(readChunks(valid), {
      ...describeContent(valid),
      contentLength: valid.length - 1,
    }),
    expectAppError("invalid"),
  );

  await assert.rejects(
    verifyText(readChunks(valid), {
      ...describeContent(valid),
      sha256: "00".repeat(32),
    }),
    expectAppError("invalid"),
  );

  const interrupted = new TypeError("owned stream failure");

  async function* readInterruptedChunks(): AsyncGenerator<Buffer> {
    yield Promise.resolve(valid.subarray(0, 3));
    throw interrupted;
  }

  await assert.rejects(
    verifyText(readInterruptedChunks(), describeContent(valid)),
    (error: unknown) => error === interrupted,
  );
});
