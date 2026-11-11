import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { Attachments } from "../src/attachments.mjs";
import { AppError, RECONCILIATION_PAGE_SIZE } from "../src/model.mjs";
import { State } from "../src/state.mjs";
import {
  BYTES,
  INPUT,
  OWNER,
  MemoryObjects,
  expectAppError,
  createFixture,
} from "./support.mjs";

await test("bounded prefix scans preserve pending and attached keys and rediscover late keys behind a completed cursor after restart", async (t) => {
  const { state, states, options } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const pending = await app.create(OWNER, INPUT);
  const live = await app.create(OWNER, INPUT);

  for (const issued of [pending, live]) {
    storage.objects.set(options.prefix + issued.attemptId, BYTES);
  }

  const attached = await app.finalize(OWNER, live.attemptId);

  const missing = Array.from(
    { length: 40 },
    (_, index) =>
      options.prefix + `orphan-${index.toString().padStart(2, "0")}`,
  );

  for (const key of missing) {
    storage.objects.set(key, BYTES);
  }

  storage.objects.set("outside/keep", BYTES);

  const first = await app.cleanup();
  assert.ok(first.deleted <= RECONCILIATION_PAGE_SIZE);
  const late = options.prefix + "000-late";
  storage.objects.set(late, BYTES);
  await app.cleanup();
  await app.cleanup();
  assert.deepEqual(storage.objects.get(late), BYTES);
  assert.ok(missing.every((key) => !storage.objects.has(key)));

  assert.deepEqual(
    storage.objects.get(options.prefix + pending.attemptId),
    BYTES,
  );

  assert.deepEqual(storage.objects.get(options.prefix + live.attemptId), BYTES);

  assert.deepEqual(await state.call("attachments", { accountId: OWNER }), [
    attached,
  ]);

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

  assert.deepEqual(await restarted.cleanup(), { deleted: 1, failed: 0 });
  assert.equal(storage.objects.has(late), false);
  assert.deepEqual(storage.objects.get("outside/keep"), BYTES);
});

await test("live attachment ownership survives removed upload history and terminal compaction remains bounded", async (t) => {
  const { state, states, options } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const live = await app.create(OWNER, INPUT);
  const key = options.prefix + live.attemptId;
  storage.objects.set(key, BYTES);
  const attached = await app.finalize(OWNER, live.attemptId);
  // Populate two bounded terminal batches without making the traversal authoritative.
  const list = storage.list.bind(storage);
  storage.list = () => Promise.reject(new AppError("unavailable"));

  for (let batch = 0; batch < 2; batch += 1) {
    const attempts = [];

    for (let index = 0; index < 16; index += 1) {
      attempts.push(await app.create(OWNER, INPUT));
    }

    await assert.rejects(
      app.cleanup(
        Math.max(...attempts.map((attempt) => attempt.expiresAt)) +
          batch * 60_000,
      ),
      expectAppError("unavailable"),
    );
  }

  await state.close();

  const database = new DatabaseSync(options.path);

  try {
    assert.equal(
      database
        .prepare(
          "SELECT count(*) AS count FROM attempts WHERE status = 'abandoned'",
        )
        .get()?.count,
      32,
    );

    database
      .prepare("DELETE FROM attempts WHERE attempt_id = ?")
      .run(live.attemptId);
  } finally {
    database.close();
  }

  const reopened = await State.open(options);
  states.push(reopened);
  storage.list = list;

  const restarted = new Attachments(
    reopened,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  await restarted.cleanup();
  assert.deepEqual(storage.objects.get(key), BYTES);

  assert.deepEqual(await reopened.call("attachments", { accountId: OWNER }), [
    attached,
  ]);

  assert.deepEqual(await restarted.finalize(OWNER, live.attemptId), attached);

  assert.equal(
    await restarted.download(OWNER, live.attemptId),
    "https://owned.invalid/" + key,
  );

  const observer = new DatabaseSync(options.path, { readOnly: true });

  try {
    assert.equal(
      observer.prepare("SELECT count(*) AS count FROM attempts").get()?.count,
      16,
    );
  } finally {
    observer.close();
  }
});

await test("database outage after listing prevents absent-key deletion and malformed pages cannot escape the owned prefix", async (t) => {
  const { state, states, options } = await createFixture(t);
  const storage = new MemoryObjects();
  const key = options.prefix + "unrecorded";
  storage.objects.set(key, BYTES);

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  storage.list = async () => {
    await state.close();
    return { keys: [key] };
  };

  await assert.rejects(app.cleanup(), expectAppError("unavailable"));
  assert.deepEqual(storage.deleted, []);
  const reopened = await State.open(options);
  states.push(reopened);

  const restarted = new Attachments(
    reopened,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  storage.list = () => Promise.resolve({ keys: [key, "outside/unsafe"] });
  await assert.rejects(restarted.cleanup(), expectAppError("unavailable"));
  assert.deepEqual(storage.deleted, []);
  assert.deepEqual(storage.objects.get(key), BYTES);
});

await test("finalization during prefix listing preserves the newly attached object", async (t) => {
  const { state, options } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const issued = await app.create(OWNER, INPUT);
  const key = options.prefix + issued.attemptId;
  storage.objects.set(key, BYTES);
  const listing = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();

  storage.list = async () => {
    listing.resolve();
    await release.promise;
    return { keys: [key] };
  };

  const cleanup = app.cleanup();
  await listing.promise;

  try {
    await app.finalize(OWNER, issued.attemptId);
  } finally {
    release.resolve();
  }

  assert.deepEqual(await cleanup, { deleted: 0, failed: 0 });
  assert.deepEqual(storage.deleted, []);
  assert.deepEqual(storage.objects.get(key), BYTES);
});

await test("empty truncated pages advance without compaction and interrupted traversal restarts before retrying unknown deletes", async (t) => {
  const { state, options } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const abandoned = await app.create(OWNER, INPUT);
  const tokens: (string | undefined)[] = [];

  storage.list = (_bucket, _prefix, token) => {
    tokens.push(token);

    if (token === undefined) {
      return Promise.resolve({ keys: [], nextContinuationToken: "next" });
    }

    return Promise.reject(
      new DOMException("owned interrupted traversal", "AbortError"),
    );
  };

  await app.cleanup(abandoned.expiresAt);

  assert.equal(
    (
      await state.call("attempt", {
        accountId: OWNER,
        attemptId: abandoned.attemptId,
      })
    ).status,
    "abandoned",
  );

  await assert.rejects(app.cleanup(), { name: "AbortError" });
  await app.cleanup();
  assert.deepEqual(tokens, [undefined, "next", undefined]);

  const key = options.prefix + "unknown";
  storage.objects.set(key, BYTES);
  storage.list = () => Promise.resolve({ keys: [key] });
  storage.loseDeleteResponse = true;
  assert.deepEqual(await app.cleanup(), { deleted: 0, failed: 1 });
  storage.objects.set(key, BYTES);
  storage.loseDeleteResponse = false;
  assert.deepEqual(await app.cleanup(), { deleted: 1, failed: 0 });
  assert.equal(storage.objects.has(key), false);
});
