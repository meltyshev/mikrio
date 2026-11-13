import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Store, StorageError } from "@mikrio/node";

function failure(code: string, outcome?: string): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, code);
    assert.equal(error.mutationOutcome, outcome);
    return true;
  };
}

await test("tagging: detached exact tags, atomic validation, cancellation and reopen", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-tagging-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });
  await using peer = await Store.open({ directory });
  const input = { bucket: "photos", key: "key" };
  await store.createBucket({ bucket: input.bucket });
  await store.putObject({ ...input, body: Buffer.from("payload") });
  const before = await store.headObject(input);

  const tags = [
    { key: "日本語", value: "𐐀" },
    { key: "Name", value: "" },
    { key: "name", value: "exact" },
  ];

  const pending = store.putObjectTagging({ ...input, tags });
  tags[0] = { key: "changed", value: "changed" };
  await pending;

  const expected = [
    { key: "Name", value: "" },
    { key: "name", value: "exact" },
    { key: "日本語", value: "𐐀" },
  ];

  const output = await peer.getObjectTagging(input);
  assert.deepEqual(output.tags, expected);
  assert.ok(Object.isFrozen(output));
  assert.ok(Object.isFrozen(output.tags));

  for (const tag of output.tags) {
    assert.ok(Object.isFrozen(tag));
  }

  const owner = await store.getOwnerInfo();

  const expectedBucketOwner =
    owner.accountId === "000000000000" ? "000000000001" : "000000000000";

  await assert.rejects(
    store.getObjectTagging({ ...input, expectedBucketOwner }),
    failure("AccessDenied"),
  );

  await assert.rejects(
    store.putObjectTagging({ ...input, expectedBucketOwner, tags: [] }),
    failure("AccessDenied", "NotCommitted"),
  );

  await assert.rejects(
    store.deleteObjectTagging({ ...input, expectedBucketOwner }),
    failure("AccessDenied", "NotCommitted"),
  );

  for (const invalid of [
    [
      { key: "same", value: "one" },
      { key: "same", value: "two" },
    ],
    [{ key: "AWS:reserved", value: "" }],
    [{ key: "key", value: "\ud800" }],
    [{ key: "key", value: "x".repeat(257) }],
  ]) {
    await assert.rejects(
      store.putObjectTagging({ ...input, tags: invalid }),
      failure("InvalidArgument", "NotCommitted"),
    );
  }

  const controller = new AbortController();
  controller.abort();

  await assert.rejects(
    store.deleteObjectTagging(input, { signal: controller.signal }),
    failure("OperationCanceled", "NotCommitted"),
  );

  assert.deepEqual(await peer.getObjectTagging(input), output);
  const after = await store.headObject(input);
  assert.equal(after.etag, before.etag);
  assert.deepEqual(after.lastModified, before.lastModified);
  assert.equal(after.tagCount, 3);

  for (const [bucket, key, code] of [
    ["photos", "missing", "NoSuchKey"],
    ["missing", "key", "NoSuchBucket"],
  ] as const) {
    await assert.rejects(
      store.getObjectTagging({ bucket, key }),
      failure(code),
    );

    await assert.rejects(
      store.putObjectTagging({ bucket, key, tags: [] }),
      failure(code, "NotCommitted"),
    );

    await assert.rejects(
      store.deleteObjectTagging({ bucket, key }),
      failure(code, "NotCommitted"),
    );
  }

  await store.close();
  await peer.close();
  await using reopened = await Store.open({ directory });
  assert.deepEqual(await reopened.getObjectTagging(input), output);
  await reopened.putObjectTagging({ ...input, tags: [] });
  assert.deepEqual((await reopened.getObjectTagging(input)).tags, []);

  for (let count = 0; count < 2; count++) {
    await reopened.deleteObjectTagging(input);
  }

  const payload = await reopened.getObject(input);

  await using payloadBody = payload.body;

  assert.deepEqual(
    await payloadBody.readAll({ maxBytes: 7 }),
    Buffer.from("payload"),
  );

  await payloadBody.close();
  await reopened.close();
  assert.deepEqual(output.tags, expected);

  await assert.rejects(
    reopened.putObjectTagging({ ...input, tags: [] }),
    failure("StoreClosed", "NotCommitted"),
  );
});
