import assert from "node:assert/strict";
import { test } from "node:test";
import { crc32 } from "node:zlib";
import { DeleteObjectCommand, GetObjectCommand } from "@aws-sdk/client-s3";
import { createPresignedPost } from "@aws-sdk/s3-presigned-post";
import { client } from "./http-client.mjs";

await test("JavaScript SDK forms retain exact fields checksums replay and native reopen bytes", async (context) => {
  const s3 = client();
  const target = { Bucket: "photos", Key: "node-post /%+雪" };

  context.after(async () => {
    try {
      await s3.send(new DeleteObjectCommand(target));
    } finally {
      s3.destroy();
    }
  });

  const payload = Buffer.from(
    Array.from({ length: 263 * 1024 }, (_, index) => index % 239),
  );

  const checksum = Buffer.alloc(4);
  checksum.writeUInt32BE(crc32(payload));

  const fields = {
    "x-amz-checksum-algorithm": "CRC32",
    "x-amz-checksum-crc32": checksum.toString("base64"),
  };

  const durable = await createPresignedPost(s3, {
    Bucket: "photos",
    Key: "node-shared",
    Expires: 60,
    Fields: fields,
    Conditions: Object.entries(fields).map(([name, value]) => ({
      [name]: value,
    })),
  });

  const send = async (
    form: typeof durable,
    body: Uint8Array<ArrayBuffer>,
  ): Promise<Response> => {
    const formBody = new FormData();

    for (const [name, value] of Object.entries(form.fields)) {
      formBody.append(name, value);
    }

    formBody.append("file", new Blob([body]), "ignored.bin");

    return fetch(form.url, {
      method: "POST",
      body: formBody,
      redirect: "manual",
    });
  };

  const response = await send(durable, payload);
  await response.arrayBuffer();
  assert.equal(response.status, 204);

  const form = await createPresignedPost(s3, {
    ...target,
    Expires: 60,
    Fields: { "Content-Type": "text/plain", success_action_status: "201" },
    Conditions: [
      { "Content-Type": "text/plain" },
      { success_action_status: "201" },
      ["content-length-range", 0, 16],
    ],
  });

  for (const content of ["", "first", "replay"]) {
    const response = await send(form, Buffer.from(content));
    const xml = await response.text();
    assert.equal(response.status, 201);
    assert.match(xml, /<PostResponse/);
    assert.ok(xml.includes(target.Key));
    const object = await s3.send(new GetObjectCommand(target));
    assert.equal(await object.Body?.transformToString(), content);
    assert.equal(object.ContentType, "text/plain");
  }

  form.fields["Content-Type"] = "tampered";
  const denied = await send(form, Buffer.from("bad"));
  const rejectedBody = await denied.text();
  assert.equal(denied.status, 400);
  assert.match(rejectedBody, /InvalidPolicyDocument/);
  const preserved = await s3.send(new GetObjectCommand(target));
  assert.equal(await preserved.Body?.transformToString(), "replay");
});
