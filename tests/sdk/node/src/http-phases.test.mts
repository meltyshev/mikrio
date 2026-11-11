import assert from "node:assert/strict";
import test from "node:test";
import {
  CompleteMultipartUploadCommand,
  CopyObjectCommand,
  GetObjectAttributesCommand,
  S3Client,
  S3ServiceException,
  UploadPartCopyCommand,
} from "@aws-sdk/client-s3";

const ETAG = '"25f9e794323b453885f5181f1b624d0b"';

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

for (const operation of ["copy", "complete"] as const) {
  void test(`${operation} parses success and errors before and after headers`, async () => {
    const s3 = client();

    try {
      for (const [key, code, status] of [
        ["deferred-success", undefined, 200],
        ["early-error", "NoSuchKey", 404],
        ["deferred-error", "InternalError", 503],
        ["stalled", "RequestTimeout", 503],
      ] as const) {
        const send = async () => {
          if (operation === "copy") {
            const result = await s3.send(
              new CopyObjectCommand({
                Bucket: "photos",
                Key: key,
                CopySource: "photos/source",
              }),
            );

            return result.CopyObjectResult?.ETag;
          }

          const result = await s3.send(
            new CompleteMultipartUploadCommand({
              Bucket: "photos",
              Key: key,
              UploadId: "synthetic-upload",
              MultipartUpload: { Parts: [{ PartNumber: 1, ETag: ETAG }] },
            }),
          );

          return result.ETag;
        };

        if (code === undefined) {
          assert.equal(await send(), ETAG);
          continue;
        }

        await assert.rejects(send, (error: unknown) => {
          assert.ok(error instanceof S3ServiceException);
          assert.equal(error.name, code);
          // The SDK maps embedded HTTP-200 errors to 503 before deserialization.
          assert.equal(error.$metadata.httpStatusCode, status);
          return true;
        });
      }
    } finally {
      s3.destroy();
    }
  });
}

void test("part copy sends a valid empty body", async () => {
  const s3 = client();

  try {
    const result = await s3.send(
      new UploadPartCopyCommand({
        Bucket: "photos",
        Key: "part-copy",
        UploadId: "synthetic-upload",
        PartNumber: 1,
        CopySource: "photos/source",
      }),
    );

    assert.equal(result.CopyPartResult?.ETag, ETAG);
  } finally {
    s3.destroy();
  }
});

void test("Attributes decode selected groups count-only and part metadata", async () => {
  const s3 = client();

  try {
    const selected = await s3.send(
      new GetObjectAttributesCommand({
        Bucket: "photos",
        Key: "etag",
        ObjectAttributes: ["ETag"],
      }),
    );

    assert.equal(selected.ETag, ETAG.slice(1, -1));

    assert.equal(
      selected.LastModified?.toISOString(),
      "2026-10-09T00:00:00.000Z",
    );

    assert.equal(selected.ObjectSize, undefined);
    assert.equal(selected.ObjectParts, undefined);

    const countOnly = await s3.send(
      new GetObjectAttributesCommand({
        Bucket: "photos",
        Key: "count-only",
        ObjectAttributes: ["ObjectParts"],
      }),
    );

    assert.deepEqual(countOnly.ObjectParts, { TotalPartsCount: 1 });

    const part = await s3.send(
      new GetObjectAttributesCommand({
        Bucket: "photos",
        Key: "part",
        ObjectAttributes: [
          "ObjectParts",
          "Checksum",
          "StorageClass",
          "ObjectSize",
        ],
        MaxParts: 1,
        PartNumberMarker: "0",
      }),
    );

    assert.equal(part.ObjectSize, 9);
    assert.equal(part.StorageClass, "STANDARD");
    assert.equal(part.ETag, undefined);

    assert.deepEqual(part.Checksum, {
      ChecksumCRC32: "y/Q5Jg==",
      ChecksumType: "FULL_OBJECT",
    });

    assert.deepEqual(part.ObjectParts, {
      TotalPartsCount: 1,
      MaxParts: 1,
      PartNumberMarker: "0",
      IsTruncated: false,
      Parts: [{ PartNumber: 1, Size: 9, ChecksumCRC32: "y/Q5Jg==" }],
    });
  } finally {
    s3.destroy();
  }
});
