import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Store, StorageError, type CorsRule } from "mikrio";

function failure(code: string, outcome?: string): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, code);
    assert.equal(error.mutationOutcome, outcome);
    return true;
  };
}

async function rejectedCalls(
  store: Store,
  bucket: string,
  corsRules: readonly CorsRule[],
  expectedBucketOwner: string | undefined,
  code: string,
): Promise<void> {
  const input = { bucket, expectedBucketOwner };

  await assert.rejects(
    store.putBucketCors({ ...input, corsRules }),
    failure(code, "NotCommitted"),
  );

  await assert.rejects(store.getBucketCors(input), failure(code));

  await assert.rejects(
    store.deleteBucketCors(input),
    failure(code, "NotCommitted"),
  );
}

await test("CORS: owned rules, optional empty fields, validation and reopen", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-cors-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });
  await using peer = await Store.open({ directory });

  await store.createBucket({ bucket: "photos" });
  const accountId = (await store.getOwnerInfo()).accountId;

  await assert.rejects(
    store.getBucketCors({ bucket: "photos" }),
    failure("NoSuchCORSConfiguration"),
  );

  const origins = ["https://app.example"];

  const first: CorsRule = {
    id: "browser-😀",
    allowedOrigins: origins,
    allowedMethods: ["GET", "PUT"],
    allowedHeaders: [],
    exposeHeaders: ["ETag"],
    maxAgeSeconds: 0,
  };

  const second: CorsRule = {
    allowedOrigins: ["*"],
    allowedMethods: ["HEAD"],
  };

  const pending = store.putBucketCors({
    bucket: "photos",
    corsRules: [first, second],
    expectedBucketOwner: accountId,
  });

  origins[0] = "https://changed.example";
  await pending;

  const output = await peer.getBucketCors({
    bucket: "photos",
    expectedBucketOwner: accountId,
  });

  assert.ok(Object.isFrozen(output));
  assert.ok(Object.isFrozen(output.corsRules));
  const rule = output.corsRules[0];
  assert.ok(rule);
  assert.ok(Object.isFrozen(rule));
  assert.ok(Object.isFrozen(rule.allowedOrigins));
  assert.deepEqual(rule.allowedOrigins, ["https://app.example"]);
  assert.deepEqual(rule.allowedMethods, ["GET", "PUT"]);
  assert.deepEqual(rule.allowedHeaders, []);
  assert.deepEqual(rule.exposeHeaders, ["ETag"]);
  assert.equal(rule.maxAgeSeconds, 0);
  assert.equal(output.corsRules[1]?.allowedHeaders, undefined);
  assert.equal(output.corsRules[1]?.maxAgeSeconds, undefined);

  for (const corsRules of [
    [],
    [first, first],
    [{ ...first, allowedMethods: ["OPTIONS"] }],
    [{ ...first, allowedHeaders: ["x-*-*"] }],
    [{ ...first, maxAgeSeconds: 2 ** 31 }],
    [{ ...first, id: "\ud800" }],
  ]) {
    await assert.rejects(
      store.putBucketCors({ bucket: "photos", corsRules }),
      failure("InvalidArgument", "NotCommitted"),
    );
  }

  await assert.rejects(
    store.putBucketCors({
      bucket: "photos",
      corsRules: [{ ...first, maxAgeSeconds: 0.5 }],
    }),
    TypeError,
  );

  for (const seconds of [-(2 ** 31), -1, 0, 2 ** 31 - 1]) {
    await store.putBucketCors({
      bucket: "photos",
      corsRules: [{ ...first, maxAgeSeconds: seconds }],
    });

    assert.equal(
      (await peer.getBucketCors({ bucket: "photos" })).corsRules[0]
        ?.maxAgeSeconds,
      seconds,
    );
  }

  await store.putBucketCors({
    bucket: "photos",
    corsRules: output.corsRules,
  });

  const wrongAccountId =
    accountId === "000000000000" ? "111111111111" : "000000000000";

  await rejectedCalls(store, "photos", [first], wrongAccountId, "AccessDenied");
  await rejectedCalls(store, "photos", [first], "", "InvalidArgument");
  await rejectedCalls(store, "missing", [first], accountId, "NoSuchBucket");
  const controller = new AbortController();
  controller.abort();

  await assert.rejects(
    store.deleteBucketCors({ bucket: "photos" }, { signal: controller.signal }),
    failure("OperationCanceled", "NotCommitted"),
  );

  assert.deepEqual(await peer.getBucketCors({ bucket: "photos" }), output);
  await store.close();
  await peer.close();

  await using reopened = await Store.open({ directory });
  assert.deepEqual(await reopened.getBucketCors({ bucket: "photos" }), output);

  for (let count = 0; count < 2; count++) {
    await reopened.deleteBucketCors({
      bucket: "photos",
      expectedBucketOwner: accountId,
    });
  }

  await assert.rejects(
    reopened.getBucketCors({ bucket: "photos" }),
    failure("NoSuchCORSConfiguration"),
  );

  await reopened.close();

  assert.equal(rule.id, "browser-😀");
  await rejectedCalls(store, "photos", [first], undefined, "StoreClosed");
});

await test("CORS: every rule field and exact configuration limits preserve the predecessor", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-cors-limits-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });

  await store.createBucket({ bucket: "photos" });

  const rule: CorsRule = {
    id: "😀".repeat(255),
    allowedOrigins: ["https://*.example.com"],
    allowedMethods: ["GET", "PUT", "HEAD", "POST", "DELETE"],
    allowedHeaders: ["x-amz-*", "Content-Type"],
    exposeHeaders: ["ETag", "Content-Disposition"],
    maxAgeSeconds: -(2 ** 31),
  };

  await store.putBucketCors({ bucket: "photos", corsRules: [rule] });
  const predecessor = await store.getBucketCors({ bucket: "photos" });
  assert.equal(predecessor.corsRules[0]?.id, rule.id);

  const invalid: Partial<CorsRule>[] = [
    { id: "😀".repeat(256) },
    { id: "control\0" },
    { allowedOrigins: [] },
    { allowedMethods: [] },
    ...["", "a b", "é", "**", "x\r\n"].map((value) => ({
      allowedOrigins: [value],
    })),
    ...["get", "OPTIONS", "PATCH"].map((value) => ({
      allowedMethods: [value],
    })),
    ...["", "x:y", "**", "é", "x\r\n"].map((value) => ({
      allowedHeaders: [value],
    })),
    ...["*", "x:y", "é"].map((value) => ({ exposeHeaders: [value] })),
    { maxAgeSeconds: -(2 ** 31) - 1 },
  ];

  for (const fields of invalid) {
    await assert.rejects(
      store.putBucketCors({
        bucket: "photos",
        corsRules: [rule, { ...rule, id: "later", ...fields }],
      }),
      failure("InvalidArgument", "NotCommitted"),
    );

    assert.deepEqual(
      await store.getBucketCors({ bucket: "photos" }),
      predecessor,
    );
  }

  for (const seconds of [
    Number.NaN,
    Number.POSITIVE_INFINITY,
    Number.NEGATIVE_INFINITY,
  ]) {
    await assert.rejects(
      store.putBucketCors({
        bucket: "photos",
        corsRules: [{ ...rule, maxAgeSeconds: seconds }],
      }),
      TypeError,
    );
  }

  const anonymous = { ...rule, id: undefined };
  const hundred = Array.from({ length: 100 }, () => anonymous);
  await store.putBucketCors({ bucket: "photos", corsRules: hundred });

  assert.equal(
    (await store.getBucketCors({ bucket: "photos" })).corsRules.length,
    100,
  );

  await assert.rejects(
    store.putBucketCors({
      bucket: "photos",
      corsRules: [...hundred, anonymous],
    }),
    failure("InvalidArgument", "NotCommitted"),
  );

  assert.equal(
    (await store.getBucketCors({ bucket: "photos" })).corsRules.length,
    100,
  );

  const shell =
    '<CORSConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><CORSRule><AllowedOrigin></AllowedOrigin><AllowedMethod>GET</AllowedMethod></CORSRule></CORSConfiguration>';

  const available = 65_536 - Buffer.byteLength(shell);

  for (const size of [available - 1, available]) {
    const boundary: CorsRule = {
      allowedOrigins: ["\\".repeat(size)],
      allowedMethods: ["GET"],
    };

    await store.putBucketCors({ bucket: "photos", corsRules: [boundary] });

    assert.deepEqual(
      (await store.getBucketCors({ bucket: "photos" })).corsRules[0]
        ?.allowedOrigins,
      boundary.allowedOrigins,
    );
  }

  await assert.rejects(
    store.putBucketCors({
      bucket: "photos",
      corsRules: [
        {
          allowedOrigins: ["\\".repeat(available + 1)],
          allowedMethods: ["GET"],
        },
      ],
    }),
    failure("InvalidArgument", "NotCommitted"),
  );

  assert.equal(
    (await store.getBucketCors({ bucket: "photos" })).corsRules[0]
      ?.allowedOrigins[0]?.length,
    available,
  );
});
