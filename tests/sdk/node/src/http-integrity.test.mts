import assert from "node:assert/strict";
import { Readable } from "node:stream";
import test from "node:test";
import {
  GetObjectCommand,
  PutObjectCommand,
  S3Client,
  S3ServiceException,
  type ChecksumAlgorithm,
} from "@aws-sdk/client-s3";
import { createPresignedPost } from "@aws-sdk/s3-presigned-post";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";

function client(): S3Client {
  const endpoint = process.env["mikrio_fixture_endpoint"];
  assert.ok(endpoint);

  return new S3Client({
    endpoint,
    region: "us-east-1",
    forcePathStyle: true,
    maxAttempts: 1,
    credentials: {
      accessKeyId: "mikrio_a00112233445566778899AABBCCDDEEFF",
      secretAccessKey: "synthetic-http-probe-secret",
    },
  });
}

void test("metadata entry boundary projects without exceeding client headers", async () => {
  const s3 = client();

  const metadata = Object.fromEntries(
    Array.from({ length: 64 }, (_, index) => [
      `key${String(index).padStart(2, "0")}`,
      "",
    ]),
  );

  try {
    const result = await s3.send(
      new GetObjectCommand({ Bucket: "photos", Key: "metadata-limit" }),
    );

    assert.ok(result.Body);
    assert.equal(await result.Body.transformToString(), "metadata-limit");
    assert.deepEqual(result.Metadata, metadata);

    await assert.rejects(
      s3.send(
        new PutObjectCommand({
          Bucket: "photos",
          Key: "metadata-over",
          Body: Buffer.alloc(0),
          Metadata: { ...metadata, extra: "" },
        }),
      ),
      (error: unknown) =>
        error instanceof S3ServiceException && error.name === "InvalidArgument",
    );
  } finally {
    s3.destroy();
  }
});

void test("explicit flexible algorithms preserve values and reject corruption", async () => {
  const s3 = client();

  const vectors: readonly (readonly [ChecksumAlgorithm, string])[] = [
    ["CRC32", "y/Q5Jg=="],
    ["CRC32C", "4waSgw=="],
    ["CRC64NVME", "rosUhgp5mIg="],
    ["SHA1", "98O8HYCOBHMq32eZZczDTKeuNEE="],
    ["SHA256", "FeKw08M4keuw8e9gnsQZQgwg4yDOlMZfvIwzEkSOsiU="],
  ];

  try {
    for (const [algorithm, encoded] of vectors) {
      const field = `Checksum${algorithm}`;

      const input = {
        Bucket: "photos",
        Key: "node-explicit",
        ChecksumAlgorithm: algorithm,
        [field]: encoded,
      };

      const result = await s3.send(
        new PutObjectCommand({ ...input, Body: Buffer.from("123456789") }),
      );

      assert.equal(Reflect.get(result, field), encoded);

      await assert.rejects(
        s3.send(
          new PutObjectCommand({ ...input, Body: Buffer.from("123456780") }),
        ),
        (error: unknown) =>
          error instanceof S3ServiceException && error.name === "BadDigest",
      );
    }
  } finally {
    s3.destroy();
  }
});

void test("SDK presign preserves its signed path", async () => {
  const s3 = client();

  try {
    const url = await getSignedUrl(
      s3,
      new PutObjectCommand({
        Bucket: "photos",
        Key: "node-presign",
        ChecksumCRC32: "y/Q5Jg==",
      }),
      { expiresIn: 60 },
    );

    const result = await fetch(url, { method: "PUT", body: "123456789" });
    await result.arrayBuffer();
    assert.equal(result.status, 200);
    const changed = url.replace("/photos/node-presign?", "/photos/changed?");
    assert.notEqual(changed, url);
    const denied = await fetch(changed, { method: "PUT", body: "123456789" });
    await denied.arrayBuffer();
    assert.equal(denied.status, 403);
  } finally {
    s3.destroy();
  }
});

void test("SDK POST policy and FormData deliver only file bytes", async () => {
  const s3 = client();

  try {
    const post = await createPresignedPost(s3, {
      Bucket: "photos",
      Key: "node-post",
      Expires: 60,
      Fields: { success_action_status: "204" },
      Conditions: [
        { success_action_status: "204" },
        ["content-length-range", 9, 9],
      ],
    });

    const form = new FormData();

    for (const [key, value] of Object.entries(post.fields)) {
      form.append(key, value);
    }

    form.append("file", new Blob(["123456789"]), "sample.txt");
    const response = await fetch(post.url, { method: "POST", body: form });
    const body = await response.arrayBuffer();
    assert.equal(response.status, 204);
    assert.equal(body.byteLength, 0);
  } finally {
    s3.destroy();
  }
});

void test("default buffer checksum is verified by the Rust core", async () => {
  const s3 = client();

  try {
    const result = await s3.send(
      new PutObjectCommand({
        Bucket: "photos",
        Key: "node-default",
        Body: Buffer.from("123456789"),
      }),
    );

    assert.equal(result.ETag, '"25f9e794323b453885f5181f1b624d0b"');
    assert.equal(result.ChecksumCRC32, "y/Q5Jg==");
  } finally {
    s3.destroy();
  }
});

void test("default Readable trailer reaches verified finish", async () => {
  const s3 = client();

  try {
    const result = await s3.send(
      new PutObjectCommand({
        Bucket: "photos",
        Key: "node-trailer",
        ContentLength: 9,
        Body: Readable.from([Buffer.from("1234"), Buffer.from("56789")]),
      }),
    );

    assert.equal(result.ETag, '"25f9e794323b453885f5181f1b624d0b"');
    assert.equal(result.ChecksumCRC32, "y/Q5Jg==");
  } finally {
    s3.destroy();
  }
});

void test("wrong checksum remains a typed SDK BadDigest error", async () => {
  const s3 = client();

  try {
    await assert.rejects(
      s3.send(
        new PutObjectCommand({
          Bucket: "photos",
          Key: "node-bad",
          Body: Buffer.from("123456789"),
          ChecksumCRC32: "AAAAAA==",
        }),
      ),
      (error: unknown) => {
        assert.ok(error instanceof S3ServiceException);
        assert.equal(error.name, "BadDigest");
        assert.equal(error.$metadata.httpStatusCode, 400);
        return true;
      },
    );
  } finally {
    s3.destroy();
  }
});
