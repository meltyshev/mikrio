import assert from "node:assert/strict";
import { test } from "node:test";
import { Upload } from "@aws-sdk/lib-storage";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import {
  AbortMultipartUploadCommand,
  CompleteMultipartUploadCommand,
  CopyObjectCommand,
  CreateMultipartUploadCommand,
  GetObjectCommand,
  GetObjectAttributesCommand,
  HeadObjectCommand,
  ListMultipartUploadsCommand,
  ListPartsCommand,
  PutObjectCommand,
  UploadPartCopyCommand,
} from "@aws-sdk/client-s3";
import { client } from "./http-client.mjs";

await test("multipart managed upload and ranged part copy preserve bytes", async (context) => {
  const s3 = client();
  context.after(() => s3.destroy());

  await s3.send(
    new PutObjectCommand({
      Bucket: "photos",
      Key: "node-shared",
      Body: Buffer.from(
        Array.from({ length: 263 * 1024 }, (_, index) => index % 239),
      ),
    }),
  );

  const content = Buffer.alloc(5 * 1024 * 1024 + 1, 120);
  content[content.length - 1] = 121;

  await new Upload({
    client: s3,
    params: {
      Bucket: "photos",
      Key: "node-managed",
      Body: content,
      Metadata: { source: "managed" },
      ChecksumAlgorithm: "SHA256",
      ChecksumType: "COMPOSITE",
    },
    partSize: 5 * 1024 * 1024,
    queueSize: 2,
  }).done();

  await s3.send(
    new CopyObjectCommand({
      Bucket: "photos",
      Key: "node-whole-copy",
      CopySource: "/photos/node-managed",
    }),
  );

  const created = await s3.send(
    new CreateMultipartUploadCommand({
      Bucket: "photos",
      Key: "node-part-copy",
    }),
  );

  assert.ok(created.UploadId);

  const upload = {
    Bucket: "photos",
    Key: "node-part-copy",
    UploadId: created.UploadId,
  };

  const copied = await s3.send(
    new UploadPartCopyCommand({
      ...upload,
      PartNumber: 1,
      CopySource: "/photos/node-managed",
      CopySourceRange: "bytes=5242880-5242880",
    }),
  );

  assert.ok(copied.CopyPartResult?.ETag);
  const listed = await s3.send(new ListPartsCommand(upload));
  assert.equal(listed.Parts?.[0]?.Size, 1);

  await s3.send(
    new CompleteMultipartUploadCommand({
      ...upload,
      MultipartUpload: {
        Parts: [{ PartNumber: 1, ETag: copied.CopyPartResult.ETag }],
      },
    }),
  );

  for (const key of ["node-managed", "node-whole-copy", "node-part-copy"]) {
    const result = await s3.send(
      new GetObjectCommand({ Bucket: "photos", Key: key }),
    );

    assert.ok(result.Body);

    assert.deepEqual(
      Buffer.from(await result.Body.transformToByteArray()),
      key === "node-part-copy" ? Buffer.from("y") : content,
    );
  }

  const page = await s3.send(
    new GetObjectAttributesCommand({
      Bucket: "photos",
      Key: "node-managed",
      ObjectAttributes: ["ObjectSize", "Checksum", "ObjectParts"],
      MaxParts: 1,
      PartNumberMarker: "0",
    }),
  );

  assert.equal(page.ObjectSize, content.length);
  assert.equal(page.ObjectParts?.TotalPartsCount, 2);
  assert.equal(page.ObjectParts?.IsTruncated, true);
  assert.equal(page.ObjectParts?.Parts?.[0]?.Size, 5 * 1024 * 1024);
  assert.equal(page.ObjectParts?.NextPartNumberMarker, "1");

  const tail = await s3.send(
    new GetObjectAttributesCommand({
      Bucket: "photos",
      Key: "node-managed",
      ObjectAttributes: ["ObjectParts"],
      MaxParts: 1,
      PartNumberMarker: "1",
    }),
  );

  assert.equal(tail.ObjectParts?.IsTruncated, false);
  assert.equal(tail.ObjectParts?.NextPartNumberMarker, "2");
  assert.equal(tail.ObjectParts?.Parts?.[0]?.Size, 1);

  const selected = await s3.send(
    new GetObjectCommand({
      Bucket: "photos",
      Key: "node-managed",
      PartNumber: 2,
      ChecksumMode: "ENABLED",
    }),
  );

  assert.equal(selected.$metadata.httpStatusCode, 206);
  assert.equal(selected.ContentRange, "bytes 5242880-5242880/5242881");

  assert.equal(
    selected.ChecksumSHA256,
    tail.ObjectParts?.Parts?.[0]?.ChecksumSHA256,
  );

  assert.ok(selected.Body);

  assert.deepEqual(
    Buffer.from(await selected.Body.transformToByteArray()),
    Buffer.from("y"),
  );

  const head = await s3.send(
    new HeadObjectCommand({
      Bucket: "photos",
      Key: "node-managed",
      PartNumber: 2,
    }),
  );

  assert.equal(head.$metadata.httpStatusCode, 206);
  assert.equal(head.PartsCount, 2);

  const signedAttributes = await getSignedUrl(
    s3,
    new GetObjectAttributesCommand({
      Bucket: "photos",
      Key: "node-managed",
      ObjectAttributes: ["ObjectSize", "ObjectParts"],
      MaxParts: 1,
      PartNumberMarker: "1",
    }),
    { expiresIn: 60 },
  );

  const attributesResponse = await fetch(signedAttributes);
  const xml = await attributesResponse.text();
  assert.equal(attributesResponse.status, 200);
  assert.ok(xml.includes("<PartNumber>2</PartNumber>"));
  assert.ok(xml.includes("<ObjectSize>5242881</ObjectSize>"));
  const changedAttributes = new URL(signedAttributes);
  changedAttributes.searchParams.set("x-amz-part-number-marker", "0");
  const deniedAttributes = await fetch(changedAttributes);
  await deniedAttributes.arrayBuffer();
  assert.equal(deniedAttributes.status, 403);

  for (const method of ["GET", "HEAD"]) {
    const input = {
      Bucket: "photos",
      Key: "node-managed",
      PartNumber: 2,
      ChecksumMode: "ENABLED" as const,
    };

    const command =
      method === "GET"
        ? new GetObjectCommand(input)
        : new HeadObjectCommand(input);

    const url = await getSignedUrl(s3, command, { expiresIn: 60 });
    const response = await fetch(url, { method });
    const body = await response.text();
    assert.equal(response.status, 206);

    assert.equal(
      response.headers.get("content-range"),
      "bytes 5242880-5242880/5242881",
    );

    assert.equal(
      response.headers.get("x-amz-checksum-sha256"),
      tail.ObjectParts?.Parts?.[0]?.ChecksumSHA256,
    );

    assert.equal(body, method === "GET" ? "y" : "");
    const changed = new URL(url);
    changed.searchParams.set("partNumber", "1");
    const denied = await fetch(changed, { method });
    await denied.arrayBuffer();
    assert.equal(denied.status, 403);
  }

  assert.equal(
    (await s3.send(new ListMultipartUploadsCommand({ Bucket: "photos" })))
      .Uploads?.length ?? 0,
    0,
  );

  const abandoned = await s3.send(
    new CreateMultipartUploadCommand({
      Bucket: "photos",
      Key: "node-abandoned",
    }),
  );

  assert.ok(abandoned.UploadId);

  await s3.send(
    new AbortMultipartUploadCommand({
      Bucket: "photos",
      Key: "node-abandoned",
      UploadId: abandoned.UploadId,
    }),
  );
});
