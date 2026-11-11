import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Store, StorageError, type LifecycleRule } from "mikrio";

function failure(
  code: string,
  mutationOutcome?: string,
): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, code);
    assert.equal(error.mutationOutcome, mutationOutcome);
    return true;
  };
}

await test("lifecycle: owned nested values, atomic replacement, authority and reopen", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-retention-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });
  await store.createBucket({ bucket: "photos" });

  await assert.rejects(
    store.getBucketLifecycleConfiguration({ bucket: "photos" }),
    failure("NoSuchLifecycleConfiguration"),
  );

  const filter = { prefix: "drafts/雪" };
  const expiration = { days: 7 };

  const first: LifecycleRule = {
    id: "retain-😀",
    status: "Enabled",
    filter,
    expiration,
    abortIncompleteMultipartUpload: { daysAfterInitiation: 1 },
  };

  const rules: LifecycleRule[] = [
    first,
    { status: "Disabled", filter: {}, expiration: { days: 2147483647 } },
    {
      id: "",
      status: "Disabled",
      filter: { prefix: "" },
      abortIncompleteMultipartUpload: { daysAfterInitiation: 2 },
    },
  ];

  const pending = store.putBucketLifecycleConfiguration({
    bucket: "photos",
    lifecycleRules: rules,
  });

  filter.prefix = "changed";
  expiration.days = 8;
  await pending;

  const output = await store.getBucketLifecycleConfiguration({
    bucket: "photos",
  });

  assert.equal(output.lifecycleRules[0]?.filter.prefix, "drafts/雪");
  assert.equal(output.lifecycleRules[0]?.expiration?.days, 7);

  assert.equal(
    output.lifecycleRules[0]?.abortIncompleteMultipartUpload
      ?.daysAfterInitiation,
    1,
  );

  assert.equal(output.lifecycleRules[1]?.filter.prefix, undefined);
  assert.equal(output.lifecycleRules[2]?.filter.prefix, "");

  for (const rule of output.lifecycleRules) {
    assert.ok(rule.id);
    assert.ok(Object.isFrozen(rule));
    assert.ok(Object.isFrozen(rule.filter));

    if (rule.expiration) {
      assert.ok(Object.isFrozen(rule.expiration));
    }

    if (rule.abortIncompleteMultipartUpload) {
      assert.ok(Object.isFrozen(rule.abortIncompleteMultipartUpload));
    }
  }

  assert.ok(Object.isFrozen(output));
  assert.ok(Object.isFrozen(output.lifecycleRules));

  const bounded = [...output.lifecycleRules];

  for (const name of ["map", Symbol.iterator]) {
    Object.defineProperty(bounded, name, {
      get() {
        assert.fail(
          "bounded lifecycle copying must not call array methods or iterators",
        );
      },
    });
  }

  let inputReads = 0;

  await store.putBucketLifecycleConfiguration({
    bucket: "photos",
    get lifecycleRules() {
      inputReads++;
      assert.equal(inputReads, 1);
      return bounded;
    },
  });

  assert.equal(inputReads, 1);

  assert.deepEqual(
    await store.getBucketLifecycleConfiguration({ bucket: "photos" }),
    output,
  );

  for (const lifecycleRules of [
    [],
    Array.from({ length: 1001 }, () => ({ ...first, id: undefined })),
    [first, first],
    [{ ...first, expiration: { days: 0 } }],
    [{ ...first, expiration: { days: 2147483648 } }],
    [{ ...first, id: "\ud800" }],
    [{ ...first, filter: { prefix: "雪".repeat(342) } }],
  ]) {
    await assert.rejects(
      store.putBucketLifecycleConfiguration({
        bucket: "photos",
        lifecycleRules,
      }),
      failure("InvalidArgument", "NotCommitted"),
    );

    assert.deepEqual(
      await store.getBucketLifecycleConfiguration({ bucket: "photos" }),
      output,
    );
  }

  // Runtime callers can supply excess nested fields; they must not disappear.
  for (const extra of [
    { ...first, filter: { prefix: "", tag: "unsupported" } },
    { ...first, expiration: { days: 1, date: "2027-01-01" } },
    { ...first, expiration: { days: 0.5 } },
  ]) {
    await assert.rejects(
      store.putBucketLifecycleConfiguration({
        bucket: "photos",
        lifecycleRules: [extra],
      }),
      TypeError,
    );
  }

  const accountId = (await store.getOwnerInfo()).accountId;
  const wrongAccountId =
    accountId === "000000000000" ? "111111111111" : "000000000000";

  for (const [bucket, expectedBucketOwner, code] of [
    ["photos", wrongAccountId, "AccessDenied"],
    ["photos", "", "InvalidArgument"],
    ["missing", accountId, "NoSuchBucket"],
  ]) {
    assert.ok(
      bucket !== undefined &&
        expectedBucketOwner !== undefined &&
        code !== undefined,
    );

    const input = { bucket, expectedBucketOwner };

    await assert.rejects(
      store.putBucketLifecycleConfiguration({
        ...input,
        lifecycleRules: output.lifecycleRules,
      }),
      failure(code, "NotCommitted"),
    );

    await assert.rejects(
      store.getBucketLifecycleConfiguration(input),
      failure(code),
    );

    await assert.rejects(
      store.deleteBucketLifecycle(input),
      failure(code, "NotCommitted"),
    );
  }

  const controller = new AbortController();
  controller.abort();

  await assert.rejects(
    store.deleteBucketLifecycle(
      { bucket: "photos" },
      { signal: controller.signal },
    ),
    failure("OperationCanceled", "NotCommitted"),
  );

  await store.close();
  await using reopened = await Store.open({ directory });

  assert.deepEqual(
    await reopened.getBucketLifecycleConfiguration({ bucket: "photos" }),
    output,
  );

  for (let count = 0; count < 2; count++) {
    await reopened.deleteBucketLifecycle({
      bucket: "photos",
      expectedBucketOwner: accountId,
    });
  }

  await assert.rejects(
    reopened.getBucketLifecycleConfiguration({ bucket: "photos" }),
    failure("NoSuchLifecycleConfiguration"),
  );

  await reopened.close();

  await assert.rejects(
    reopened.putBucketLifecycleConfiguration({
      bucket: "photos",
      lifecycleRules: [first],
    }),
    failure("StoreClosed", "NotCommitted"),
  );

  assert.equal(output.lifecycleRules[0]?.id, "retain-😀");
});
