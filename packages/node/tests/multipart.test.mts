import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Store, StorageError } from "@mikrio/node";

await test("multipart: resumable parts, copy, failure and owned results", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-multipart-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });
  await store.createBucket({ bucket: "photos" });

  await store.putObject({
    bucket: "photos",
    key: "source",
    body: Buffer.from("abc"),
  });

  const copied = await store.copyObject({
    bucket: "photos",
    key: "copied",
    copySource: { bucket: "photos", key: "source" },
    metadataDirective: "REPLACE",
    metadata: { origin: "copy" },
  });

  const created = await store.createMultipartUpload({
    bucket: "photos",
    key: "object",
    metadata: { origin: "initiation" },
    tags: [{ key: "kind", value: "multipart" }],
  });

  assert.equal(created.checksumAlgorithm, "CRC64NVME");
  await store.close();
  await using peer = await Store.open({ directory });

  const upload = {
    bucket: "photos",
    key: "object",
    uploadId: created.uploadId,
  };

  const active = await peer.listMultipartUploads({
    bucket: "photos",
    maxUploads: 1,
  });

  assert.equal(active.isTruncated, false);
  assert.equal(active.uploads[0]?.uploadId, created.uploadId);

  const part = await peer.uploadPart({
    ...upload,
    partNumber: 7,
    body: Buffer.from("abc"),
    contentLength: 3n,
    checksumCrc32: "NSRBwg==",
  });

  assert.equal(part.checksumCrc64Nvme, "BeXKuz/B+us=");
  const failure = new Error("producer failure");

  async function* broken(): AsyncGenerator<Uint8Array> {
    yield await Promise.resolve(Buffer.from("before failure"));
    throw failure;
  }

  await assert.rejects(
    peer.uploadPart({ ...upload, partNumber: 7, body: broken() }),
    (error: unknown) => {
      assert.ok(error instanceof StorageError);
      assert.equal(error.code, "BodyReadFailed");
      assert.equal(error.mutationOutcome, "NotCommitted");
      assert.equal(error.cause, failure);
      return true;
    },
  );

  const page = await peer.listParts(upload);
  assert.equal(page.parts[0]?.etag, part.etag);
  assert.ok(Object.isFrozen(page.parts));
  assert.ok(Object.isFrozen(page.parts[0]));

  const replacement = await peer.uploadPartCopy({
    ...upload,
    partNumber: 7,
    copySource: { bucket: "photos", key: "copied" },
    copySourceIfMatch: copied.etag,
  });

  const completed = await peer.completeMultipartUpload({
    ...upload,
    parts: [{ partNumber: 7, etag: replacement.etag }],
    contentLength: 3n,
    checksumCrc64Nvme: "BeXKuz/B+us=",
  });

  assert.equal(completed.checksumType, "FULL_OBJECT");

  const countOnly = await peer.getObjectAttributes({
    bucket: "photos",
    key: "object",
    objectAttributes: ["ObjectParts"],
    maxParts: 1,
    partNumberMarker: 10000,
  });

  assert.equal(countOnly.objectParts?.nextPartNumberMarker, undefined);

  assert.deepEqual(countOnly.objectParts, {
    totalPartsCount: 1,
    maxParts: 1,
    partNumberMarker: 10000,
    parts: [],
    isTruncated: false,
  });

  const payload = await peer.getObject({ bucket: "photos", key: "object" });

  await using payloadBody = payload.body;

  assert.deepEqual(
    await payloadBody.readAll({ maxBytes: 3 }),
    Buffer.from("abc"),
  );

  await payloadBody.close();

  assert.equal(
    (await peer.headObject({ bucket: "photos", key: "object" })).metadata
      .origin,
    "initiation",
  );

  assert.equal(
    (await peer.getObjectTagging({ bucket: "photos", key: "object" })).tags[0]
      ?.value,
    "multipart",
  );

  const abandoned = await peer.createMultipartUpload({
    bucket: "photos",
    key: "abandoned",
  });

  const abort = {
    bucket: "photos",
    key: "abandoned",
    uploadId: abandoned.uploadId,
  };

  const controller = new AbortController();
  controller.abort();

  await assert.rejects(
    peer.abortMultipartUpload(abort, { signal: controller.signal }),
    (error: unknown) =>
      error instanceof StorageError &&
      error.code === "OperationCanceled" &&
      error.mutationOutcome === "NotCommitted",
  );

  await peer.abortMultipartUpload(abort);

  assert.equal(
    (await peer.listMultipartUploads({ bucket: "photos" })).uploads.length,
    0,
  );

  await peer.close();
  assert.equal(page.parts[0]?.partNumber, 7);
  assert.equal(active.uploads[0]?.key, "object");
});

await test("Attributes and part reads preserve bounded detached projections", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-layout-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory });

  await store.createBucket({ bucket: "photos" });

  const upload = await store.createMultipartUpload({
    bucket: "photos",
    key: "object",
    checksumAlgorithm: "SHA256",
    checksumType: "COMPOSITE",
  });

  const identity = {
    bucket: "photos",
    key: "object",
    uploadId: upload.uploadId,
  };

  const part = await store.uploadPart({
    ...identity,
    partNumber: 1,
    body: Buffer.from("abc"),
  });

  await store.completeMultipartUpload({
    ...identity,
    parts: [
      { partNumber: 1, etag: part.etag, checksumSha256: part.checksumSha256 },
    ],
  });

  const output = await store.getObjectAttributes({
    bucket: "photos",
    key: "object",
    objectAttributes: ["ObjectSize", "Checksum", "ObjectParts"],
    maxParts: 1,
  });

  assert.equal(output.objectSize, 3n);
  assert.equal(output.etag, undefined);
  assert.equal(output.objectParts?.totalPartsCount, 1);

  assert.equal(
    output.objectParts?.parts[0]?.checksumSha256,
    part.checksumSha256,
  );

  assert.equal(output.objectParts?.nextPartNumberMarker, undefined);
  assert.equal(output.objectParts?.isTruncated, false);
  assert.ok(Object.isFrozen(output));
  assert.ok(Object.isFrozen(output.checksum));
  assert.ok(Object.isFrozen(output.objectParts?.parts));
  assert.ok(Object.isFrozen(output.objectParts?.parts[0]));
  let selectorReads = 0;
  const selectorArray: ["ETag"] = ["ETag"];

  Object.defineProperty(selectorArray, Symbol.iterator, {
    get() {
      assert.fail("bounded array decoding must not invoke custom iterators");
    },
  });

  const snapshot = await store.getObjectAttributes({
    bucket: "photos",
    key: "object",
    get objectAttributes() {
      selectorReads++;
      assert.equal(selectorReads, 1);
      return selectorArray;
    },
  });

  assert.ok(snapshot.etag);
  assert.equal(selectorReads, 1);

  const selected = await store.getObject({
    bucket: "photos",
    key: "object",
    partNumber: 1,
    checksumMode: "ENABLED",
  });

  await using selectedBody = selected.body;

  assert.equal(selected.contentRange, "bytes 0-2/3");
  assert.equal(selected.partsCount, 1);
  assert.equal(selected.checksumSha256, part.checksumSha256);

  assert.deepEqual(
    await selectedBody.readAll({ maxBytes: 3 }),
    Buffer.from("abc"),
  );

  await selectedBody.close();

  const head = await store.headObject({
    bucket: "photos",
    key: "object",
    partNumber: 1,
  });

  assert.equal(head.partsCount, 1);

  await assert.rejects(
    store.headObject({ bucket: "photos", key: "object", partNumber: 2 }),
    (error: unknown) =>
      error instanceof StorageError && error.code === "InvalidPartNumber",
  );

  for (const partNumber of [0, 1.5, NaN, Infinity, 2 ** 40]) {
    await assert.rejects(
      store.headObject({ bucket: "photos", key: "object", partNumber }),
      (error: unknown) =>
        error instanceof StorageError && error.code === "InvalidArgument",
    );
  }

  await assert.rejects(
    store.getObjectAttributes({
      bucket: "photos",
      key: "object",
      objectAttributes: ["ETag", "ETag"],
    }),
    (error: unknown) =>
      error instanceof StorageError && error.code === "InvalidArgument",
  );

  for (const maxParts of [0, 1001, 1.5, NaN, Infinity, 2 ** 40]) {
    await assert.rejects(
      store.getObjectAttributes({
        bucket: "photos",
        key: "object",
        objectAttributes: ["ObjectSize"],
        maxParts,
      }),
      (error: unknown) =>
        error instanceof StorageError && error.code === "InvalidArgument",
    );
  }

  for (const partNumberMarker of [-1, 10001, 1.5, Infinity, 2 ** 40]) {
    await assert.rejects(
      store.getObjectAttributes({
        bucket: "photos",
        key: "object",
        objectAttributes: ["ObjectParts"],
        partNumberMarker,
      }),
      (error: unknown) =>
        error instanceof StorageError && error.code === "InvalidArgument",
    );
  }

  const controller = new AbortController();
  controller.abort();

  await assert.rejects(
    store.getObjectAttributes(
      { bucket: "photos", key: "object", objectAttributes: ["ETag"] },
      { signal: controller.signal },
    ),
    (error: unknown) =>
      error instanceof StorageError &&
      error.code === "OperationCanceled" &&
      error.mutationOutcome === undefined,
  );

  await store.close();
  assert.equal(output.objectParts?.parts[0]?.size, 3n);

  {
    await using reopened = await Store.open({ directory });

    assert.deepEqual(
      (
        await reopened.getObjectAttributes({
          bucket: "photos",
          key: "object",
          objectAttributes: ["ObjectParts"],
          maxParts: 1,
        })
      ).objectParts,
      output.objectParts,
    );
  }
});

await test("multipart: rejected operations preserve configuration, parts and object", async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-operation-errors-"));

  context.after(async () => {
    if (context.passed) {
      await rm(directory, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${directory}`);
    }
  });

  await using store = await Store.open({ directory, region: "eu-west-1" });
  await store.createBucket({ bucket: "photos" });

  await store.putObject({
    bucket: "photos",
    key: "kept",
    body: Buffer.from("original"),
    metadata: { origin: "kept" },
  });

  const created = await store.createMultipartUpload({
    bucket: "photos",
    key: "kept",
  });

  const upload = { bucket: "photos", key: "kept", uploadId: created.uploadId };

  const part = await store.uploadPart({
    ...upload,
    partNumber: 1,
    body: Buffer.from("abc"),
  });

  const operations: [
    string,
    () => Promise<unknown>,
    string,
    string | undefined,
  ][] = [
    [
      "create bucket",
      () => store.createBucket({ bucket: "photos" }),
      "BucketAlreadyOwnedByYou",
      "NotCommitted",
    ],
    [
      "copy",
      () =>
        store.copyObject({
          bucket: "photos",
          key: "kept",
          copySource: { bucket: "photos", key: "kept" },
          metadataDirective: "REPLACE",
          ifNoneMatch: "*",
        }),
      "PreconditionFailed",
      "NotCommitted",
    ],
    [
      "create upload",
      () => store.createMultipartUpload({ bucket: "missing", key: "kept" }),
      "NoSuchBucket",
      "NotCommitted",
    ],
    [
      "list parts",
      () => store.listParts({ ...upload, key: "other" }),
      "NoSuchUpload",
      undefined,
    ],
    [
      "list uploads",
      () => store.listMultipartUploads({ bucket: "missing" }),
      "NoSuchBucket",
      undefined,
    ],
    [
      "complete",
      () =>
        store.completeMultipartUpload({
          ...upload,
          parts: [
            { partNumber: 1, etag: '"00000000000000000000000000000000"' },
          ],
        }),
      "InvalidPart",
      "NotCommitted",
    ],
    [
      "copy part",
      () =>
        store.uploadPartCopy({
          ...upload,
          partNumber: 1,
          copySource: { bucket: "photos", key: "missing" },
        }),
      "NoSuchKey",
      "NotCommitted",
    ],
    [
      "get policy",
      () => store.getBucketPolicy({ bucket: "missing" }),
      "NoSuchBucket",
      undefined,
    ],
    [
      "delete policy",
      () => store.deleteBucketPolicy({ bucket: "missing" }),
      "NoSuchBucket",
      "NotCommitted",
    ],
    [
      "delete public access block",
      () => store.deletePublicAccessBlock({ bucket: "missing" }),
      "NoSuchBucket",
      "NotCommitted",
    ],
    [
      "get ownership",
      () => store.getBucketOwnershipControls({ bucket: "missing" }),
      "NoSuchBucket",
      undefined,
    ],
    [
      "get acl",
      () => store.getBucketAcl({ bucket: "missing" }),
      "NoSuchBucket",
      undefined,
    ],
  ];

  for (const [name, operation, code, outcome] of operations) {
    await assert.rejects(operation, (error: unknown) => {
      assert.ok(error instanceof StorageError, name);
      assert.equal(error.code, code, name);
      assert.equal(error.mutationOutcome, outcome, name);
      return true;
    });
  }

  await store.close();
  await using reopened = await Store.open({ directory, region: "eu-west-1" });
  const previous = await reopened.getObject({ bucket: "photos", key: "kept" });
  await using previousBody = previous.body;
  assert.deepEqual(previous.metadata, { origin: "kept" });

  assert.deepEqual(
    await previousBody.readAll({ maxBytes: 8 }),
    Buffer.from("original"),
  );

  const parts = await reopened.listParts(upload);
  assert.equal(parts.parts.length, 1);
  assert.equal(parts.parts[0]?.etag, part.etag);

  await reopened.completeMultipartUpload({
    ...upload,
    parts: [{ partNumber: 1, etag: part.etag }],
  });

  const completed = await reopened.getObject({ bucket: "photos", key: "kept" });
  await using completedBody = completed.body;

  assert.deepEqual(
    await completedBody.readAll({ maxBytes: 3 }),
    Buffer.from("abc"),
  );
});
