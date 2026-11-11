import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { Readable } from "node:stream";
import test from "node:test";
import {
  DeleteObjectCommand,
  GetObjectCommand,
  HeadObjectCommand,
  PutObjectCommand,
  S3ServiceException,
} from "@aws-sdk/client-s3";
import { client } from "./http-client.mjs";

await test("native and boto3 objects are readable with verified stored metadata", async () => {
  const s3 = client();

  try {
    const native = await s3.send(
      new GetObjectCommand({ Bucket: "photos", Key: "native-seed" }),
    );

    assert.ok(native.Body);
    assert.equal(await native.Body.transformToString(), "native-to-sdk");
    assert.deepEqual(native.Metadata, { title: "=?UTF-8?B?Y2Fmw6k=?=" });

    const response = await s3.send(
      new GetObjectCommand({
        Bucket: "photos",
        Key: "boto-shared",
        ChecksumMode: "ENABLED",
      }),
    );

    assert.ok(response.Body);

    const expected = Buffer.from(
      Array.from({ length: 257 * 1024 }, (_, index) => index % 251),
    );

    assert.deepEqual(
      Buffer.from(await response.Body.transformToByteArray()),
      expected,
    );

    assert.deepEqual(response.Metadata, { producer: "boto3" });

    assert.equal(
      response.ETag,
      `"${createHash("md5").update(expected).digest("hex")}"`,
    );

    assert.ok(response.ChecksumCRC32);
  } finally {
    s3.destroy();
  }
});

await test("default Readable checksum trailer publishes bytes for native reopen", async () => {
  const s3 = client();
  const target = { Bucket: "photos", Key: "node-shared" };

  const payload = Buffer.from(
    Array.from({ length: 263 * 1024 }, (_, index) => index % 239),
  );

  try {
    const put = await s3.send(
      new PutObjectCommand({
        ...target,
        Body: Readable.from(
          (function* () {
            for (let offset = 0; offset < payload.length; offset += 17003) {
              yield payload.subarray(offset, offset + 17003);
            }
          })(),
        ),
        ContentLength: payload.length,
        Metadata: { producer: "node" },
        ContentType: "application/octet-stream",
      }),
    );

    assert.ok(put.ChecksumCRC32);

    const get = await s3.send(
      new GetObjectCommand({ ...target, ChecksumMode: "ENABLED" }),
    );

    assert.ok(get.Body);

    assert.deepEqual(
      Buffer.from(await get.Body.transformToByteArray()),
      payload,
    );

    const head = await s3.send(
      new HeadObjectCommand({ ...target, ChecksumMode: "ENABLED" }),
    );

    assert.equal(head.ContentLength, payload.length);
    assert.equal(head.ChecksumCRC32, put.ChecksumCRC32);
    assert.equal(head.ContentEncoding, undefined);
    assert.deepEqual(head.Metadata, { producer: "node" });
  } finally {
    s3.destroy();
  }
});

await test("conditional replacement and delete return SDK errors without changing bytes", async () => {
  const s3 = client();
  const target = { Bucket: "photos", Key: "node-condition" };

  try {
    const put = await s3.send(
      new PutObjectCommand({
        ...target,
        Body: Buffer.from("original"),
        IfNoneMatch: "*",
      }),
    );

    await assert.rejects(
      s3.send(
        new PutObjectCommand({
          ...target,
          Body: Buffer.from("replacement"),
          IfNoneMatch: "*",
        }),
      ),
      (error: unknown) =>
        error instanceof S3ServiceException &&
        error.name === "PreconditionFailed",
    );

    const response = await s3.send(new GetObjectCommand(target));
    assert.ok(response.Body);
    assert.equal(await response.Body.transformToString(), "original");

    await assert.rejects(
      s3.send(new DeleteObjectCommand({ ...target, IfMatch: '"wrong"' })),
      (error: unknown) =>
        error instanceof S3ServiceException &&
        error.name === "PreconditionFailed",
    );

    await s3.send(new DeleteObjectCommand({ ...target, IfMatch: put.ETag }));

    await assert.rejects(
      s3.send(new GetObjectCommand(target)),
      (error: unknown) =>
        error instanceof S3ServiceException && error.name === "NoSuchKey",
    );
  } finally {
    s3.destroy();
  }
});
