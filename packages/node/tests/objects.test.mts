import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { getEventListeners } from "node:events";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rename,
  rm,
  rmdir,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  setImmediate as yieldTurn,
  setTimeout as delay,
} from "node:timers/promises";
import { test, type TestContext } from "node:test";
import { Store, StorageError, type MutationOutcome } from "@mikrio/node";

async function fixture(
  context: TestContext,
): Promise<{ store: Store; path: string }> {
  const path = await mkdtemp(join(tmpdir(), "mikrio-node-objects-"));
  let store: Store | undefined = undefined;

  context.after(async () => {
    await store?.close();

    if (context.passed) {
      await rm(path, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${path}`);
    }
  });

  store = await Store.open({ directory: path });

  await store.createBucket({ bucket: "bucket" });
  return { store, path };
}

await test("objects: batch deletion preserves duplicate conditions quiet errors and frozen results", async (context) => {
  const { store } = await fixture(context);

  const put = await store.putObject({
    bucket: "bucket",
    key: "same",
    body: Buffer.from("payload"),
  });

  const output = await store.deleteObjects({
    bucket: "bucket",
    objects: [
      { key: "same", etag: '"wrong"' },
      { key: "same", etag: put.etag },
      { key: "same", etag: put.etag },
      { key: "same" },
    ],
  });

  assert.deepEqual(
    output.deleted.map((item) => item.key),
    ["same", "same"],
  );

  assert.deepEqual(
    output.errors.map((item) => item.code),
    ["PreconditionFailed", "NoSuchKey"],
  );

  for (const value of [
    output,
    output.deleted,
    output.errors,
    ...output.deleted,
    ...output.errors,
  ]) {
    assert.ok(Object.isFrozen(value));
  }

  const quiet = await store.deleteObjects({
    bucket: "bucket",
    objects: [{ key: "missing" }, { key: "missing", etag: "*" }],
    quiet: true,
  });

  assert.deepEqual(quiet.deleted, []);
  assert.equal(quiet.errors[0]?.code, "NoSuchKey");

  for (const objects of [
    [],
    Array.from({ length: 1001 }, () => ({ key: "missing" })),
  ]) {
    await assert.rejects(
      store.deleteObjects({ bucket: "bucket", objects }),
      (error: unknown) => {
        assert.ok(error instanceof StorageError);
        assert.equal(error.code, "InvalidArgument");
        assert.equal(error.mutationOutcome, "NotCommitted");
        assert.equal(error.partialResult, undefined);
        return true;
      },
    );
  }

  for (const invalid of [
    { bucket: "bucket", objects: [{ key: "missing", versionId: "1" }] },
    { bucket: "bucket", objects: new Array<unknown>(1) },
    { bucket: "bucket", objects: [{ key: "missing" }], quiet: 1 },
  ]) {
    // @ts-expect-error Deliberate invalid host values.
    await assert.rejects(store.deleteObjects(invalid), TypeError);
  }

  await store.close();
  assert.equal(output.errors[0]?.key, "same");
});

await test("objects: batch cancellation settles indexed progress after an observed commit", async (context) => {
  const { store } = await fixture(context);

  await store.putObject({
    bucket: "bucket",
    key: "first",
    body: Buffer.from("payload"),
  });

  const controller = new AbortController();
  const reason = new Error("caller canceled the batch");

  const pending = store.deleteObjects(
    {
      bucket: "bucket",
      quiet: true,
      objects: [
        { key: "first" },
        ...Array.from({ length: 999 }, () => ({ key: "missing" })),
      ],
    },
    { signal: controller.signal },
  );

  const settled = pending.then(
    () => undefined,
    (error: unknown) => error,
  );

  const deadline = performance.now() + 5000;

  for (;;) {
    try {
      await store.headObject({ bucket: "bucket", key: "first" });
    } catch (error: unknown) {
      assert.ok(error instanceof StorageError);
      assert.equal(error.code, "NoSuchKey");
      break;
    }

    assert.ok(performance.now() < deadline);
  }

  controller.abort(reason);
  const error = await settled;
  assert.ok(error instanceof StorageError);
  assert.equal(error.code, "OperationCanceled");
  assert.equal(error.cause, reason);
  assert.equal(error.mutationOutcome, "Partial");
  const progress = error.partialResult;
  assert.ok(progress);
  assert.equal(progress.items.length, 1000);
  assert.equal(progress.items[0]?.mutationOutcome, "Committed");
  assert.equal(progress.items.at(-1)?.mutationOutcome, "NotCommitted");
  assert.equal(progress.items.at(-1)?.error, undefined);

  assert.deepEqual(
    progress.items.map((item) => item.index),
    Array.from({ length: 1000 }, (_, index) => index),
  );

  await store.close();
  assert.equal(progress.items[0]?.key, "first");

  for (const value of [progress, progress.items, ...progress.items]) {
    assert.ok(Object.isFrozen(value));
  }

  for (const item of progress.items) {
    if (item.error) {
      assert.ok(Object.isFrozen(item.error));
    }
  }

  assert.equal(getEventListeners(controller.signal, "abort").length, 0);
});

await test("objects: bucket pages remain live scoped and detached after reopen", async (context) => {
  const { store, path } = await fixture(context);

  for (const bucket of ["bucket-a", "bucket-b"]) {
    await store.createBucket({ bucket });
  }

  const first = await store.listBuckets({ prefix: "bucket", maxBuckets: 1 });

  assert.deepEqual(
    first.buckets.map((bucket) => bucket.name),
    ["bucket"],
  );

  assert.ok(first.buckets[0]?.creationDate instanceof Date);
  assert.equal(first.buckets[0]?.bucketRegion, "us-east-1");
  assert.ok(first.continuationToken);

  assert.ok(
    Object.isFrozen(first) &&
      Object.isFrozen(first.buckets) &&
      Object.isFrozen(first.buckets[0]),
  );

  await store.close();

  {
    await using reopened = await Store.open({ directory: path });

    await reopened.deleteBucket({ bucket: "bucket-a" });
    await reopened.createBucket({ bucket: "bucket-c" });

    const last = await reopened.listBuckets({
      prefix: "bucket",
      maxBuckets: 2,
      continuationToken: first.continuationToken,
    });

    assert.deepEqual(
      last.buckets.map((bucket) => bucket.name),
      ["bucket-b", "bucket-c"],
    );

    assert.equal(last.continuationToken, undefined);

    await assert.rejects(
      reopened.listBuckets({ continuationToken: first.continuationToken }),
      code("InvalidArgument"),
    );

    for (const maxBuckets of [0, -1, 10001, 0.5, NaN, Infinity, 2 ** 64]) {
      await assert.rejects(
        reopened.listBuckets({ maxBuckets }),
        code("InvalidArgument"),
      );
    }
  }

  assert.equal(first.buckets[0]?.name, "bucket");
});

await test("objects: ranges and all response overrides preserve stored metadata", async (context) => {
  const { store } = await fixture(context);

  await store.putObject({
    bucket: "bucket",
    key: "range",
    body: Buffer.from("testcontent"),
    contentType: "text/plain",
  });

  const responseExpires = new Date("2037-10-21T07:28:00Z");

  const input = {
    bucket: "bucket",
    key: "range",
    range: "bytes=4-7",
    checksumMode: "ENABLED" as const,
    responseContentType: "application/pdf",
    responseContentEncoding: "identity",
    responseContentDisposition: "attachment",
    responseContentLanguage: "",
    responseCacheControl: "no-store",
    responseExpires,
  };

  const get = await store.getObject(input);

  await using getBody = get.body;
  const head = await store.headObject(input);

  for (const output of [get, head]) {
    assert.equal(output.contentLength, 4n);
    assert.equal(output.contentRange, "bytes 4-7/11");
    assert.equal(output.contentType, "application/pdf");
    assert.equal(output.contentEncoding, "identity");
    assert.equal(output.contentDisposition, "attachment");
    assert.equal(output.contentLanguage, "");
    assert.equal(output.cacheControl, "no-store");
    assert.deepEqual(output.expires, responseExpires);
    assert.equal(output.checksumCrc64Nvme, undefined);
    assert.ok(Object.isFrozen(output));
  }

  assert.deepEqual(await getBody.readAll({ maxBytes: 4 }), Buffer.from("cont"));

  await getBody.close();

  for (const read of [
    store.getObject.bind(store),
    store.headObject.bind(store),
  ]) {
    await assert.rejects(
      read({ ...input, range: "bytes=99-" }),
      code("InvalidRange"),
    );

    for (const responseContentType of ["bad\r\nheader", "\ud800"]) {
      await assert.rejects(
        read({ ...input, responseContentType }),
        code("InvalidArgument"),
      );
    }
  }

  const original = await store.headObject({
    bucket: "bucket",
    key: "range",
    checksumMode: "ENABLED",
  });

  assert.equal(original.contentType, "text/plain");
  assert.equal(original.contentLength, 11n);
  assert.equal(original.contentRange, undefined);
  assert.ok(original.checksumCrc64Nvme);
});

await test("objects: listing pages preserve groups, scope, bigint sizes and detached values after reopen", async (context) => {
  const { store, path } = await fixture(context);

  for (const key of ["a", "b/one", "b/two", "c+雪"]) {
    await store.putObject({ bucket: "bucket", key, body: Buffer.from("abc") });
  }

  const first = await store.listObjectsV2({
    bucket: "bucket",
    delimiter: "/",
    maxKeys: 2,
  });

  assert.deepEqual(
    first.contents.map((object) => object.key),
    ["a"],
  );

  assert.deepEqual(first.commonPrefixes, [{ prefix: "b/" }]);
  assert.equal(first.contents[0]?.size, 3n);
  assert.equal(first.contents[0]?.etag, '"900150983cd24fb0d6963f7d28e17f72"');
  assert.ok(first.contents[0]?.lastModified instanceof Date);
  assert.ok(first.isTruncated);
  assert.ok(first.nextContinuationToken);
  await store.close();

  {
    await using reopened = await Store.open({ directory: path });

    const final = await reopened.listObjectsV2({
      bucket: "bucket",
      delimiter: "/",
      maxKeys: 1,
      continuationToken: first.nextContinuationToken,
    });

    assert.deepEqual(
      final.contents.map((object) => object.key),
      ["c+雪"],
    );

    assert.deepEqual(final.commonPrefixes, []);
    assert.equal(final.isTruncated, false);
    assert.equal(final.nextContinuationToken, undefined);

    await assert.rejects(
      reopened.listObjectsV2({
        bucket: "bucket",
        continuationToken: first.nextContinuationToken,
      }),
      code("InvalidArgument"),
    );

    for (const maxKeys of [-1, 0.5, NaN, Infinity, 1001, 2 ** 64]) {
      await assert.rejects(
        reopened.listObjectsV2({ bucket: "bucket", maxKeys }),
        code("InvalidArgument"),
      );
    }

    const zero = await reopened.listObjectsV2({ bucket: "bucket", maxKeys: 0 });
    assert.deepEqual(zero.contents, []);
    assert.equal(zero.isTruncated, false);

    await assert.rejects(
      reopened.listObjectsV2({ bucket: "bucket", prefix: "\ud800" }),
      code("InvalidArgument"),
    );
  }

  assert.equal(first.contents[0]?.key, "a");
});

function code(
  expected: string,
  outcome?: MutationOutcome,
): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, expected);
    assert.equal(error.mutationOutcome, outcome);
    return true;
  };
}

function barrier(): { promise: Promise<void>; resolve: () => void } {
  let resolve: (() => void) | undefined;

  const promise = new Promise<void>((complete) => {
    resolve = complete;
  });

  assert.ok(resolve);
  return { promise, resolve };
}

async function eventually(
  check: () => boolean | Promise<boolean>,
  timeoutMs = 10_000,
): Promise<void> {
  const deadline = performance.now() + timeoutMs;

  while (!(await check())) {
    assert.ok(
      performance.now() < deadline,
      "owned observation exceeded its watchdog",
    );

    await delay(20);
  }
}

async function exists(path: string): Promise<boolean> {
  try {
    await access(path);
    return true;
  } catch (error: unknown) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") {
      return false;
    }

    throw error;
  }
}

await test("objects: idle maintenance diagnostics survive throwing reentrant and blocked host output", async (context) => {
  const previous = console.error;
  const release = barrier();
  const messages: string[] = [];
  let reentered = false;
  let blocked = false;
  let finished = false;

  // Exercise a nonstandard Promise-returning replacement of the host's void sink.
  // eslint-disable-next-line @typescript-eslint/no-misused-promises
  console.error = async (message: unknown): Promise<void> => {
    assert.equal(typeof message, "string");
    messages.push(String(message));

    if (messages.length === 1) {
      assert.ok(store);
      await store.headBucket({ bucket: "bucket" });
      reentered = true;
      throw new Error("observer-secret-canary");
    }

    blocked = true;
    await release.promise;
    finished = true;
  };

  context.after(() => {
    console.error = previous;
    release.resolve();
  });

  const opened = await fixture(context);
  const store = opened.store;
  await using peer = await Store.open({ directory: opened.path });

  await store.putObject({
    bucket: "bucket",
    key: "secret-key-canary",
    body: new TextEncoder().encode("secret-body-canary"),
  });

  await using selected = (
    await store.getObject({ bucket: "bucket", key: "secret-key-canary" })
  ).body;

  const files = (
    await readdir(join(opened.path, "objects"), {
      recursive: true,
      withFileTypes: true,
    })
  )
    .filter((entry) => entry.isFile())
    .map((entry) => join(entry.parentPath, entry.name));

  assert.equal(files.length, 1);
  const blob = files[0];
  assert.ok(blob);
  const retained = blob + "-secret-path-canary";
  await store.deleteObject({ bucket: "bucket", key: "secret-key-canary" });
  await rename(blob, retained);
  await mkdir(blob);
  await selected.close();

  try {
    // No foreground call triggers the failure: native maintenance retries the path.
    await eventually(() => reentered && blocked);
    assert.match(messages[0] ?? "", /work=cleanup/);
    assert.match(messages[1] ?? "", /observer_failures=1/);

    for (const message of messages) {
      assert.ok(!message.includes("canary"));
      assert.ok(!message.includes(opened.path));
    }

    assert.equal(await readFile(retained, "utf8"), "secret-body-canary");
    await rmdir(blob);
    await rename(retained, blob);
    await eventually(async () => !(await exists(blob)));
    await store.close();

    await peer.putObject({
      bucket: "bucket",
      key: "peer",
      body: Uint8Array.of(7),
    });

    await peer.close();

    assert.equal(
      finished,
      false,
      "close must not await a pending host callback",
    );

    assert.equal(messages.length, 2, "only one host callback may be in flight");
  } finally {
    console.error = previous;
    release.resolve();

    if (await exists(retained)) {
      await rmdir(blob);
      await rename(retained, blob);
    }
  }
});

await test("objects: held deleted body reports bounded cleanup sample", async (context) => {
  const previous = console.error;
  const messages: string[] = [];

  console.error = (message: unknown): void => {
    assert.equal(typeof message, "string");
    messages.push(String(message));
  };

  context.after(() => {
    console.error = previous;
  });

  const { store, path } = await fixture(context);

  await store.putObject({
    bucket: "bucket",
    key: "private-key-canary",
    body: new TextEncoder().encode("private-body-canary"),
  });

  await using body = (
    await store.getObject({ bucket: "bucket", key: "private-key-canary" })
  ).body;

  await store.deleteObject({ bucket: "bucket", key: "private-key-canary" });

  await eventually(
    () =>
      messages.some(
        (message) =>
          message.includes("bounded cleanup sample") &&
          message.includes("sampled_blobs=1 observed_file_bytes=19 ") &&
          !message.includes("blocked_shard_attempts=0 "),
      ),
    20_000,
  );

  const sample = messages
    .filter((message) => message.includes("bounded cleanup sample"))
    .at(-1);

  assert.ok(sample);
  assert.match(sample, /unobserved_blobs=0 more_pending=false/);
  assert.match(sample, /oldest_sample_blob_age_ms=Some\(\d+\)/);

  const files = await readdir(join(path, "objects"), {
    recursive: true,
    withFileTypes: true,
  });

  assert.equal(files.filter((entry) => entry.isFile()).length, 1);

  assert.deepEqual(
    new Uint8Array(await body.readAll({ maxBytes: 19 })),
    new TextEncoder().encode("private-body-canary"),
  );

  for (const message of messages) {
    assert.ok(!message.includes("canary"));
    assert.ok(!message.includes(path));
  }
});

await test("objects: installed metadata checksums guarded reads and reopen", async (context) => {
  const { store, path } = await fixture(context);
  const bytes = new Uint8Array(1_048_777).fill(57);
  const checksum = createHash("sha256").update(bytes).digest("base64");
  const expires = new Date("2028-02-03T04:05:06.123Z");

  const put = await store.putObject({
    bucket: "bucket",
    key: "literal/%2F/雪",
    body: bytes,
    contentLength: BigInt(bytes.byteLength),
    contentType: "application/example",
    metadata: { MiXeD: "snow 雪", ["__proto__"]: "literal" },
    tags: [
      { key: "Case", value: "" },
      { key: "case", value: "one" },
    ],
    expires,
    checksumAlgorithm: "SHA256",
    checksumSha256: checksum,
  });

  assert.equal(put.checksumSha256, checksum);

  const head = await store.headObject({
    bucket: "bucket",
    key: "literal/%2F/雪",
    checksumMode: "ENABLED",
  });

  assert.equal(head.contentLength, BigInt(bytes.length));
  assert.equal(head.etag, put.etag);
  assert.equal(head.tagCount, 2);

  assert.deepEqual(head.metadata, {
    mixed: "snow 雪",
    ["__proto__"]: "literal",
  });

  assert.equal(Object.getPrototypeOf(head.metadata), Object.prototype);
  assert.equal(head.expires?.getTime(), expires.getTime());
  assert.equal(head.checksumSha256, checksum);
  assert.equal(head.checksumType, "FULL_OBJECT");
  assert.equal(head.contentEncoding, undefined);
  assert.ok(Object.isFrozen(head));
  assert.ok(Object.isFrozen(head.metadata));
  await store.close();
  await using reopened = await Store.open({ directory: path });

  const output = await reopened.getObject({
    bucket: "bucket",
    key: "literal/%2F/雪",
  });

  await using body = output.body;

  assert.deepEqual(
    new Uint8Array((await body.readChunk({ maxBytes: 7 })) ?? []),
    bytes.slice(0, 7),
  );

  assert.deepEqual(
    new Uint8Array(await body.readAll({ maxBytes: bytes.length - 7 })),
    bytes.slice(7),
  );

  assert.equal(await body.readChunk({ maxBytes: 1 }), null);
  assert.equal((await body.readAll({ maxBytes: 0 })).length, 0);
  await body.close();
  await assert.rejects(body.readChunk({ maxBytes: 1 }), code("BodyClosed"));
});

await test("objects: selected reader survives replacement deletion and peer close", async (context) => {
  const { store, path } = await fixture(context);
  const bytes = new Uint8Array(300_000).fill(11);
  await store.putObject({ bucket: "bucket", key: "key", body: bytes });
  await using peer = await Store.open({ directory: path });

  await using body = (await store.getObject({ bucket: "bucket", key: "key" }))
    .body;

  assert.deepEqual(
    new Uint8Array((await body.readChunk({ maxBytes: 3 })) ?? []),
    bytes.slice(0, 3),
  );

  await peer.putObject({
    bucket: "bucket",
    key: "key",
    body: Uint8Array.of(9),
  });

  assert.deepEqual(
    await peer.deleteObject({ bucket: "bucket", key: "key" }),
    {},
  );

  await peer.close();

  assert.deepEqual(
    new Uint8Array(await body.readAll({ maxBytes: bytes.length })),
    bytes.slice(3),
  );

  await store.putObject({
    bucket: "bucket",
    key: "new",
    body: Uint8Array.of(7),
  });
});

await test("objects: mutable large chunks drain before next pull and empty chunks yield", async (context) => {
  const { store } = await fixture(context);
  const chunk = new Uint8Array(400_000).fill(1);

  async function* chunks(): AsyncGenerator<Uint8Array> {
    await yieldTurn();
    yield new Uint8Array();
    yield chunk;
    chunk.fill(2);
    yield chunk;
    yield new Uint8Array();
  }

  await store.putObject({ bucket: "bucket", key: "key", body: chunks() });

  await using body = (await store.getObject({ bucket: "bucket", key: "key" }))
    .body;

  const actual = new Uint8Array(await body.readAll({ maxBytes: 800_000 }));
  assert.deepEqual(actual.slice(0, 400_000), new Uint8Array(400_000).fill(1));
  assert.deepEqual(actual.slice(400_000), new Uint8Array(400_000).fill(2));
});

await test("objects: source failure keeps its cause and the predecessor", async (context) => {
  const { store } = await fixture(context);
  const cause = new Error("controlled producer failure");
  let finalized = false;

  async function* chunks(): AsyncGenerator<Uint8Array> {
    try {
      await yieldTurn();
      yield Uint8Array.of(1);
      throw cause;
    } finally {
      finalized = true;
    }
  }

  await store.putObject({
    bucket: "bucket",
    key: "key",
    body: Uint8Array.of(9),
  });

  await assert.rejects(
    store.putObject({ bucket: "bucket", key: "key", body: chunks() }),
    (error: unknown) => {
      code("BodyReadFailed", "NotCommitted")(error);
      assert.ok(error instanceof StorageError);
      assert.equal(error.cause, cause);
      return true;
    },
  );

  assert.equal(finalized, true);

  await using body = (await store.getObject({ bucket: "bucket", key: "key" }))
    .body;

  assert.deepEqual(
    new Uint8Array(await body.readAll({ maxBytes: 1 })),
    Uint8Array.of(9),
  );
});

await test("objects: validation never pulls and conditions report authoritative outcomes", async (context) => {
  const { store } = await fixture(context);
  let pulls = 0;

  async function* chunks(): AsyncGenerator<Uint8Array> {
    await yieldTurn();
    pulls += 1;
    yield Uint8Array.of(1);
  }

  await assert.rejects(
    store.putObject({
      bucket: "bucket",
      key: "key",
      body: chunks(),
      contentLength: 5n * 1024n ** 3n + 1n,
    }),
    code("InvalidArgument", "NotCommitted"),
  );

  assert.equal(pulls, 0);

  await assert.rejects(
    store.putObject({
      bucket: "bucket",
      key: "\ud800",
      body: Uint8Array.of(1),
    }),
    code("InvalidArgument", "NotCommitted"),
  );

  const put = await store.putObject({
    bucket: "bucket",
    key: "key",
    body: Uint8Array.of(7),
    ifNoneMatch: "*",
  });

  await assert.rejects(
    store.getObject({ bucket: "bucket", key: "key", ifNoneMatch: put.etag }),
    code("NotModified"),
  );

  await assert.rejects(
    store.deleteObject({ bucket: "bucket", key: "key", ifMatch: '"other"' }),
    code("PreconditionFailed", "NotCommitted"),
  );

  await assert.rejects(
    store.deleteBucket({ bucket: "bucket" }),
    code("BucketNotEmpty", "NotCommitted"),
  );

  await store.deleteObject({ bucket: "bucket", key: "key", ifMatch: put.etag });
  assert.deepEqual(await store.deleteBucket({ bucket: "bucket" }), {});
});

await test("objects: GET signal stays attached through the body and detaches at EOF", async (context) => {
  const { store } = await fixture(context);

  await store.putObject({
    bucket: "bucket",
    key: "key",
    body: Uint8Array.of(1, 2, 3),
  });

  const abort = new AbortController();
  const baseline = getEventListeners(abort.signal, "abort").length;

  await using body = (
    await store.getObject(
      { bucket: "bucket", key: "key" },
      { signal: abort.signal },
    )
  ).body;

  assert.equal(getEventListeners(abort.signal, "abort").length, baseline + 1);
  assert.equal((await body.readAll({ maxBytes: 3 })).length, 3);
  assert.equal(getEventListeners(abort.signal, "abort").length, baseline);
  abort.abort();
  assert.equal(await body.readChunk({ maxBytes: 1 }), null);

  const next = new AbortController();

  await using selected = (
    await store.getObject(
      { bucket: "bucket", key: "key" },
      { signal: next.signal },
    )
  ).body;

  next.abort();
  await assert.rejects(selected.readChunk({ maxBytes: 1 }), code("BodyClosed"));
  assert.equal(getEventListeners(next.signal, "abort").length, 0);
});

await test("objects: cancellation returns while a foreign producer is stalled and late bytes cannot publish", async (context) => {
  const { store, path } = await fixture(context);
  const entered = barrier();
  const released = barrier();
  const finalized = barrier();

  async function* chunks(): AsyncGenerator<Uint8Array> {
    try {
      entered.resolve();
      await released.promise;
      yield Uint8Array.of(99);
    } finally {
      finalized.resolve();
    }
  }

  const controller = new AbortController();

  const pending = store.putObject(
    { bucket: "bucket", key: "key", body: chunks() },
    { signal: controller.signal },
  );

  const failed = assert.rejects(pending, (error: unknown) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, "OperationCanceled");

    assert.ok(
      error.mutationOutcome === "Unknown" ||
        error.mutationOutcome === "NotCommitted",
    );

    return true;
  });

  await entered.promise;
  controller.abort(new Error("canceled by test"));
  await failed;
  await store.close();
  released.resolve();
  await finalized.promise;
  await using reopened = await Store.open({ directory: path });

  await assert.rejects(
    reopened.headObject({ bucket: "bucket", key: "key" }),
    code("NoSuchKey"),
  );
});

await test("objects: empty streams and collection bounds keep terminal states distinct", async (context) => {
  const { store } = await fixture(context);

  await store.putObject({
    bucket: "bucket",
    key: "empty",
    body: new Uint8Array(),
    contentLength: 0n,
  });

  await using empty = (
    await store.getObject({ bucket: "bucket", key: "empty" })
  ).body;

  assert.equal((await empty.readAll({ maxBytes: 0 })).length, 0);
  assert.equal(await empty.readChunk({ maxBytes: 1 }), null);

  await store.putObject({
    bucket: "bucket",
    key: "key",
    body: Uint8Array.of(1, 2, 3),
  });

  await using body = (await store.getObject({ bucket: "bucket", key: "key" }))
    .body;

  await assert.rejects(body.readAll({ maxBytes: 2 }), code("BodyTooLarge"));
  await assert.rejects(body.readChunk({ maxBytes: 1 }), code("BodyClosed"));
});

await test("objects: endless empty chunks allow cancellation progress", async (context) => {
  const { store } = await fixture(context);
  const started = barrier();

  // eslint-disable-next-line @typescript-eslint/require-await -- tests wrapper fairness for a source whose next resolves immediately.
  async function* chunks(): AsyncGenerator<Uint8Array> {
    while (true) {
      started.resolve();
      yield new Uint8Array();
    }
  }

  const abort = new AbortController();

  const pending = store.putObject(
    { bucket: "bucket", key: "key", body: chunks() },
    { signal: abort.signal },
  );

  const failed = assert.rejects(
    pending,
    (error: unknown) =>
      error instanceof StorageError && error.code === "OperationCanceled",
  );

  await started.promise;
  await yieldTurn();
  abort.abort();
  await failed;
});

await test(
  "objects: producer admission is released after rejection and terminal pumping",
  { timeout: 120_000 },
  async (context) => {
    const { store } = await fixture(context);
    let pulls = 0;
    let released = 0;

    async function* source(): AsyncGenerator<Uint8Array> {
      pulls++;

      try {
        await yieldTurn();
        yield Buffer.from("x");
      } finally {
        released++;
      }
    }

    // Native preflight occurs after creating the private producer owner. Repeated
    // refusal must not retain its admission until a later garbage collection.
    for (let index = 0; index < 64; index++) {
      await assert.rejects(
        store.putObject({
          bucket: "bucket",
          key: "reused",
          body: source(),
          contentLength: -1n,
        }),
        code("InvalidArgument", "NotCommitted"),
      );
    }

    assert.equal(pulls, 0);

    for (let index = 0; index < 40; index++) {
      await store.putObject({
        bucket: "bucket",
        key: "reused",
        body: source(),
        contentLength: 1n,
      });
    }

    assert.equal(pulls, 40);
    assert.equal(released, 40);
    const result = await store.getObject({ bucket: "bucket", key: "reused" });
    await using body = result.body;
    assert.deepEqual(await body.readAll({ maxBytes: 1 }), Buffer.from("x"));
  },
);

await test("objects: throwing iterator cleanup lookup preserves failures and releases admission", async (context) => {
  const { store } = await fixture(context);
  const primary = new Error("producer failure");
  let cleanup: unknown;
  let finalized = 0;

  const source: AsyncIterable<Uint8Array> = {
    [Symbol.asyncIterator]() {
      return {
        async next(): Promise<IteratorResult<Uint8Array>> {
          await yieldTurn();
          throw primary;
        },
        get return(): () => Promise<IteratorResult<Uint8Array>> {
          finalized++;
          throw cleanup;
        },
      };
    },
  };

  await store.putObject({
    bucket: "bucket",
    key: "key",
    body: Uint8Array.of(9),
  });

  for (let index = 0; index < 40; index++) {
    cleanup = index % 2 === 0 ? new Error("cleanup lookup failure") : undefined;

    await assert.rejects(
      store.putObject({ bucket: "bucket", key: "key", body: source }),
      (error: unknown) => {
        code("BodyReadFailed", "NotCommitted")(error);
        assert.ok(error instanceof StorageError);
        assert.ok(error.cause instanceof AggregateError);
        assert.deepEqual(error.cause.errors, [primary, cleanup]);
        return true;
      },
    );
  }

  assert.equal(finalized, 40);

  await using body = (await store.getObject({ bucket: "bucket", key: "key" }))
    .body;

  assert.deepEqual(await body.readAll({ maxBytes: 1 }), Buffer.from([9]));
});
