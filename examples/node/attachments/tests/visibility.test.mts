import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { Attachments } from "../src/attachments.mjs";
import {
  CLEANUP_RETRY_MS,
  type Visibility as DesiredVisibility,
} from "../src/model.mjs";
import type { ObjectLocation } from "../src/storage.mjs";
import { State } from "../src/state.mjs";
import { Visibility } from "../src/visibility.mjs";
import {
  BYTES,
  INPUT,
  OWNER,
  OTHER,
  MemoryObjects,
  expectAppError,
  createFixture,
} from "./support.mjs";

await test("visibility intent survives restart and stale acknowledgments cannot clear newer intent", async (t) => {
  const { state, options, states } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const issued = await app.create(OWNER, INPUT);
  storage.objects.set(options.prefix + issued.attemptId, BYTES);
  await app.finalize(OWNER, issued.attemptId);

  const visibility = new Visibility(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  await assert.rejects(
    visibility.change(OTHER, issued.attemptId, { visibility: "public" }),
    expectAppError("notFound"),
  );

  await assert.rejects(
    visibility.change(OWNER, issued.attemptId, {
      visibility: "public",
      tags: [],
    }),
    expectAppError("invalid"),
  );

  const pending = await visibility.change(OWNER, issued.attemptId, {
    visibility: "public",
  });

  assert.equal(pending.publication, "pending");

  const old = await state.call("visibilityWork", {
    attemptId: issued.attemptId,
  });

  await visibility.change(OWNER, issued.attemptId, { visibility: "private" });

  await state.call("finishVisibility", {
    attemptId: issued.attemptId,
    revision: old.revision,
    succeeded: true,
    now: Date.now(),
  });

  assert.equal(
    (
      await state.call("attachment", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    )?.publication,
    "pending",
  );

  await state.close();
  const reopened = await State.open(options);
  states.push(reopened);

  const resumed = new Visibility(
    reopened,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  assert.deepEqual(await resumed.reconcile(), { applied: 1, failed: 0 });

  assert.equal(
    storage.visibility.get(options.prefix + issued.attemptId),
    "private",
  );

  assert.equal(
    (
      await reopened.call("attachment", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    )?.publication,
    "applied",
  );
});

await test("recurring visibility reconciliation repairs a late stale public write after acknowledged private intent", async (t) => {
  const { state, options } = await createFixture(t);

  class DelayedObjects extends MemoryObjects {
    completeLateWrite: (() => void) | undefined;

    override async setVisibility(
      object: ObjectLocation,
      visibility: DesiredVisibility,
    ): Promise<void> {
      if (visibility === "public" && !this.completeLateWrite) {
        this.completeLateWrite = () => {
          this.visibility.set(object.key, "public");
        };

        throw new Error("owned timed-out request still running remotely");
      }

      await super.setVisibility(object, visibility);
    }
  }

  const storage = new DelayedObjects();

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
  await app.finalize(OWNER, issued.attemptId);

  const visibility = new Visibility(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  const now = Date.now();
  await visibility.change(OWNER, issued.attemptId, { visibility: "public" });
  assert.deepEqual(await visibility.reconcile(now), { applied: 0, failed: 1 });

  assert.equal(
    (
      await state.call("attachment", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    )?.publication,
    "failed",
  );

  await visibility.change(OWNER, issued.attemptId, { visibility: "private" });
  await visibility.reconcile(now);
  assert.equal(storage.visibility.get(key), "private");
  assert.ok(storage.completeLateWrite);
  storage.completeLateWrite();
  assert.equal(storage.visibility.get(key), "public");

  assert.deepEqual(await visibility.reconcile(now + CLEANUP_RETRY_MS), {
    applied: 1,
    failed: 0,
  });

  assert.equal(storage.visibility.get(key), "private");
  assert.deepEqual(storage.objects.get(key), BYTES);
});

await test("visibility is opt-in and unaccepted app state or a failed app commit never authorizes publication", async (t) => {
  const { state, options, states } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const issued = await app.create(OWNER, INPUT);

  const enabled = new Visibility(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  await assert.rejects(
    enabled.change(OWNER, issued.attemptId, { visibility: "public" }),
    expectAppError("notFound"),
  );

  storage.objects.set(options.prefix + issued.attemptId, BYTES);
  await app.finalize(OWNER, issued.attemptId);

  for (const [optIn, authoritative] of [
    [false, true],
    [true, false],
  ] as const) {
    const paused = new Visibility(
      state,
      storage,
      options.bucket,
      options.prefix,
      optIn,
      authoritative,
    );

    await assert.rejects(
      paused.change(OWNER, issued.attemptId, { visibility: "public" }),
      expectAppError("unavailable"),
    );

    await assert.rejects(paused.reconcile(), expectAppError("unavailable"));
  }

  await state.close();
  const db = new DatabaseSync(options.path);

  try {
    db.exec(
      "CREATE TRIGGER reject_visibility BEFORE UPDATE OF desired_visibility ON attachments BEGIN SELECT RAISE(ABORT, 'owned visibility failure'); END",
    );
  } finally {
    db.close();
  }

  const reopened = await State.open(options);
  states.push(reopened);

  const resumed = new Visibility(
    reopened,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  await assert.rejects(
    resumed.change(OWNER, issued.attemptId, { visibility: "public" }),
    expectAppError("unavailable"),
  );

  assert.equal(storage.visibility.size, 0);

  assert.equal(
    (
      await reopened.call("attachment", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    )?.visibility,
    "private",
  );
});

await test("live ownership and pending visibility survive reopen", async (t) => {
  const { state, options, states } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const issued = await app.create(OWNER, INPUT);
  storage.objects.set(options.prefix + issued.attemptId, BYTES);
  const attached = await app.finalize(OWNER, issued.attemptId);
  await state.close();

  const reopened = await State.open(options);
  states.push(reopened);

  assert.deepEqual(await reopened.call("attachments", { accountId: OWNER }), [
    attached,
  ]);

  assert.equal(
    await reopened.call("orphan", { key: options.prefix + issued.attemptId }),
    false,
  );

  assert.equal(
    (
      await reopened.call("attempt", {
        attemptId: issued.attemptId,
        accountId: OWNER,
      })
    ).status,
    "attached",
  );
});

await test("visibility batches are bounded and concurrent reconciliations share one owner", async (t) => {
  const { state, options } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  for (let i = 0; i < 17; i += 1) {
    const issued = await app.create(OWNER, INPUT);
    storage.objects.set(options.prefix + issued.attemptId, BYTES);
    await app.finalize(OWNER, issued.attemptId);
  }

  const visibility = new Visibility(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  const now = Date.now();
  const first = visibility.reconcile(now);
  assert.equal(visibility.reconcile(now), first);
  assert.deepEqual(await first, { applied: 16, failed: 0 });
  assert.deepEqual(await visibility.reconcile(now), { applied: 1, failed: 0 });
  assert.equal(storage.visibility.size, 17);
});

await test("visibility restart reconciles current intent when storage succeeds but the app acknowledgment is lost", async (t) => {
  const { state, options, states } = await createFixture(t);

  class ClosingObjects extends MemoryObjects {
    afterTag: (() => Promise<void>) | undefined;

    override async setVisibility(
      object: ObjectLocation,
      visibility: DesiredVisibility,
    ): Promise<void> {
      await super.setVisibility(object, visibility);
      await this.afterTag?.();
    }
  }

  const storage = new ClosingObjects();

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
  await app.finalize(OWNER, issued.attemptId);

  const visibility = new Visibility(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  await visibility.change(OWNER, issued.attemptId, { visibility: "public" });
  await state.close();
  await assert.rejects(visibility.reconcile(), expectAppError("unavailable"));
  assert.equal(storage.visibility.size, 0);

  const reopened = await State.open(options);
  states.push(reopened);

  const resumed = new Visibility(
    reopened,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  storage.afterTag = () => reopened.close();
  await assert.rejects(resumed.reconcile(), expectAppError("unavailable"));
  assert.equal(storage.visibility.get(key), "public");

  const recovered = await State.open(options);
  states.push(recovered);

  const pending = await recovered.call("attachment", {
    accountId: OWNER,
    attemptId: issued.attemptId,
  });

  assert.equal(pending?.publication, "pending");
  assert.equal(pending?.visibility, "public");
  storage.afterTag = undefined;

  const current = new Visibility(
    recovered,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  await current.change(OWNER, issued.attemptId, { visibility: "private" });
  assert.deepEqual(await current.reconcile(), { applied: 1, failed: 0 });
  assert.equal(storage.visibility.get(key), "private");
  assert.deepEqual(storage.objects.get(key), BYTES);

  // An externally deleted object cannot turn a failed tag write into applied
  // intent, and the attachment's live cleanup ownership remains independent.
  storage.objects.delete(key);
  await current.change(OWNER, issued.attemptId, { visibility: "public" });
  assert.deepEqual(await current.reconcile(), { applied: 0, failed: 1 });

  assert.equal(
    (
      await recovered.call("attachment", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    )?.publication,
    "failed",
  );

  assert.equal(await recovered.call("orphan", { key }), false);
});

await test("an unknown app format is refused without changing live attachment ownership", async (t) => {
  const { state, options, states } = await createFixture(t);
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const issued = await app.create(OWNER, INPUT);
  storage.objects.set(options.prefix + issued.attemptId, BYTES);
  await app.finalize(OWNER, issued.attemptId);
  await state.close();
  let db = new DatabaseSync(options.path);

  try {
    db.exec("PRAGMA user_version = 2;");
  } finally {
    db.close();
  }

  await assert.rejects(State.open(options), expectAppError("unavailable"));
  db = new DatabaseSync(options.path);

  try {
    assert.equal(db.prepare("PRAGMA user_version").get()?.user_version, 2);

    assert.equal(
      db
        .prepare("PRAGMA table_info(attachments)")
        .all()
        .some((row) => row.name === "desired_visibility"),
      true,
    );

    assert.equal(
      db
        .prepare("SELECT status FROM attempts WHERE attempt_id = ?")
        .get(issued.attemptId)?.status,
      "attached",
    );

    assert.equal(
      db.prepare("SELECT count(*) AS total FROM attachments").get()?.total,
      1,
    );

    db.exec("PRAGMA user_version = 1;");
  } finally {
    db.close();
  }

  const reopened = await State.open(options);
  states.push(reopened);

  assert.equal(
    await reopened.call("orphan", { key: options.prefix + issued.attemptId }),
    false,
  );

  assert.equal(
    (
      await reopened.call("attachment", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    )?.visibility,
    "private",
  );
});
