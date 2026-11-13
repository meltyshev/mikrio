import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Store } from "@mikrio/node";
import {
  DeleteObjectCommand,
  PutObjectCommand,
  S3Client,
  S3ServiceException,
} from "@aws-sdk/client-s3";
import { Attachments } from "../src/attachments.mjs";
import { createBackendPolicy } from "../src/local.mjs";
import { State } from "../src/state.mjs";
import { S3Storage } from "../src/storage.mjs";
import { BYTES, INPUT, OWNER, expectAppError } from "./support.mjs";

await test("ordinary SDK through the public listener preserves signed create-only uploads, validates real bytes and denies uploader deletion", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-attachment-sdk-"));
  const resources = new AsyncDisposableStack();

  t.after(async () => {
    await resources.disposeAsync();

    if (t.passed) {
      await rm(directory, { recursive: true });
    } else {
      t.diagnostic(`retained failed SDK state: ${directory}`);
    }
  });

  const store = await Store.open({ directory: join(directory, "store") });
  resources.defer(() => store.close());
  const bucket = "attachments";
  await store.createBucket({ bucket });
  const uploader = await store.createPrincipal({ name: "uploader" });
  const backend = await store.createPrincipal({ name: "backend" });

  await store.putPrincipalPolicy({
    principalId: uploader.principalId,
    document: JSON.stringify({
      Version: "2012-10-17",
      Statement: {
        Effect: "Allow",
        Action: "s3:PutObject",
        Resource: "arn:aws:s3:::attachments/private/*",
        Condition: { StringEquals: { "s3:if-none-match": "*" } },
      },
    }),
  });

  const backendStatement = {
    Effect: "Allow",
    Action: ["s3:GetObject", "s3:DeleteObject"],
    Resource: "arn:aws:s3:::attachments/private/*",
  };

  const backendPolicy = JSON.stringify({
    Version: "2012-10-17",
    Statement: [
      backendStatement,
      {
        Effect: "Allow",
        Action: "s3:ListBucket",
        Resource: "arn:aws:s3:::attachments",
        Condition: { StringEquals: { "s3:prefix": "private/" } },
      },
    ],
  });

  await store.putPrincipalPolicy({
    principalId: backend.principalId,
    document: backendPolicy,
  });

  const uploadKey = await store.createAccessKey({
    principalId: uploader.principalId,
  });

  const backendKey = await store.createAccessKey({
    principalId: backend.principalId,
  });

  const listener = await store.listen({ host: "127.0.0.1", port: 0 });
  resources.defer(() => listener.close());

  const options = {
    endpoint: listener.endpoint,
    region: listener.region,
    uploader: {
      accessKeyId: uploadKey.accessKey.accessKeyId,
      secretAccessKey: uploadKey.secretAccessKey,
    },
    backend: {
      accessKeyId: backendKey.accessKey.accessKeyId,
      secretAccessKey: backendKey.secretAccessKey,
    },
  };

  const storage = new S3Storage(options);
  resources.defer(() => storage.close());

  const state = await State.open({
    path: join(directory, "app.sqlite3"),
    bucket,
    prefix: "private/",
    storageId: randomUUID(),
    initialize: true,
  });

  resources.defer(() => state.close());

  await state.call("addAccount", {
    accountId: OWNER,
    username: "owner",
    salt: "00".repeat(16),
    passwordHash: "00".repeat(32),
  });

  const app = new Attachments(state, storage, bucket, "private/", true);
  const issued = await app.create(OWNER, INPUT);

  const expectAccessDenied = (error: unknown): boolean => {
    assert.ok(error instanceof S3ServiceException);
    assert.equal(error.name, "AccessDenied");
    assert.equal(error.$metadata.httpStatusCode, 403);
    return true;
  };

  // Prefix-constrained listing does not authorize a GET's absent-key disclosure:
  // that operation has no listing-prefix context.
  await assert.rejects(
    app.finalize(OWNER, issued.attemptId),
    expectAccessDenied,
  );

  await store.putPrincipalPolicy({
    principalId: backend.principalId,
    document: JSON.stringify({
      Version: "2012-10-17",
      Statement: [
        backendStatement,
        {
          Effect: "Allow",
          Action: "s3:ListBucket",
          Resource: "arn:aws:s3:::attachments",
        },
      ],
    }),
  });

  await assert.rejects(
    app.finalize(OWNER, issued.attemptId),
    expectAppError("notFound"),
  );

  await store.putPrincipalPolicy({
    principalId: backend.principalId,
    document: backendPolicy,
  });

  assert.equal(
    (
      await state.call("attempt", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    ).status,
    "pending",
  );

  assert.deepEqual(await state.call("attachments", { accountId: OWNER }), []);

  const signed = new URL(issued.upload.url).searchParams
    .get("X-Amz-SignedHeaders")
    ?.split(";");

  assert.ok(signed?.includes("if-none-match"));
  assert.ok(signed?.includes("content-length"));
  assert.equal(issued.upload.headers["if-none-match"], "*");

  const put = async (headers: Record<string, string>): Promise<number> => {
    const response = await fetch(issued.upload.url, {
      method: "PUT",
      headers,
      body: BYTES,
    });

    await response.arrayBuffer();
    return response.status;
  };

  const omitted = { ...issued.upload.headers };
  delete omitted["if-none-match"];
  assert.equal(await put(omitted), 403);

  assert.equal(
    await put({ ...issued.upload.headers, "if-none-match": '"changed"' }),
    403,
  );

  assert.equal(await put(issued.upload.headers), 200);

  await store.putPrincipalPolicy({
    principalId: backend.principalId,
    document: JSON.stringify({
      Version: "2012-10-17",
      Statement: { ...backendStatement, Effect: "Deny" },
    }),
  });

  await assert.rejects(
    app.finalize(OWNER, issued.attemptId),
    expectAccessDenied,
  );

  assert.equal(
    (
      await state.call("attempt", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    ).status,
    "pending",
  );

  assert.deepEqual(await state.call("attachments", { accountId: OWNER }), []);

  const unverified = await store.getObject({
    bucket,
    key: "private/" + issued.attemptId,
  });

  try {
    assert.deepEqual(
      Buffer.from(await unverified.body.readAll({ maxBytes: BYTES.length })),
      BYTES,
    );
  } finally {
    await unverified.body.close();
  }

  await store.putPrincipalPolicy({
    principalId: backend.principalId,
    document: backendPolicy,
  });

  // The application receives no upload acknowledgment; GET determines the bytes.
  const attached = await app.finalize(OWNER, issued.attemptId);
  assert.equal(attached.sha256, INPUT.sha256);
  assert.equal(await put(issued.upload.headers), 412);

  const wrongLength = await fetch(issued.upload.url, {
    method: "PUT",
    headers: issued.upload.headers,
    body: Buffer.concat([BYTES, Buffer.from("extra")]),
  });

  await wrongLength.arrayBuffer();
  assert.equal(wrongLength.status, 403);
  const downloaded = await fetch(await app.download(OWNER, issued.attemptId));
  assert.equal(downloaded.status, 200);
  assert.deepEqual(Buffer.from(await downloaded.arrayBuffer()), BYTES);
  const originalDownload = await app.download(OWNER, issued.attemptId);

  await store.setPrincipalEnabled({
    principalId: backend.principalId,
    isEnabled: false,
  });

  const revokedDownload = await fetch(originalDownload);
  const deniedBody = await revokedDownload.text();
  assert.equal(revokedDownload.status, 403);
  assert.equal(revokedDownload.headers.get("etag"), null);
  assert.equal(deniedBody.includes(BYTES.toString()), false);

  await store.setPrincipalEnabled({
    principalId: backend.principalId,
    isEnabled: true,
  });

  const restoredDownload = await fetch(originalDownload);
  assert.equal(restoredDownload.status, 200);
  assert.deepEqual(Buffer.from(await restoredDownload.arrayBuffer()), BYTES);

  const unsigned = await fetch(
    `${listener.endpoint}/${bucket}/private/${issued.attemptId}`,
  );

  await unsigned.arrayBuffer();
  assert.equal(unsigned.status, 403);

  const client = new S3Client({
    endpoint: listener.endpoint,
    region: listener.region,
    credentials: options.uploader,
    forcePathStyle: true,
    maxAttempts: 1,
  });

  resources.defer(() => client.destroy());

  await assert.rejects(
    client.send(
      new DeleteObjectCommand({
        Bucket: bucket,
        Key: "private/" + issued.attemptId,
      }),
    ),
    expectAccessDenied,
  );

  await assert.rejects(
    client.send(
      new PutObjectCommand({
        Bucket: bucket,
        Key: "private/unconditional",
        Body: BYTES,
      }),
    ),
    expectAccessDenied,
  );

  const binary = Buffer.from([0x50, 0x4b, 0, 3]);

  const invalid = await app.create(OWNER, {
    filename: "false.txt",
    contentLength: binary.length,
    sha256: createHash("sha256").update(binary).digest("hex"),
  });

  const response = await fetch(invalid.upload.url, {
    method: "PUT",
    headers: invalid.upload.headers,
    body: binary,
  });

  await response.arrayBuffer();
  assert.equal(response.status, 200);

  await assert.rejects(
    app.finalize(OWNER, invalid.attemptId),
    expectAppError("invalid"),
  );

  await store.putPrincipalPolicy({
    principalId: backend.principalId,
    document: JSON.stringify({
      Version: "2012-10-17",
      Statement: { ...backendStatement, Action: "s3:GetObject" },
    }),
  });

  await assert.rejects(app.cleanup(invalid.expiresAt), expectAccessDenied);

  assert.equal(
    (
      await state.call("attempt", {
        accountId: OWNER,
        attemptId: invalid.attemptId,
      })
    ).status,
    "abandoned",
  );

  const retained = await store.getObject({
    bucket,
    key: "private/" + invalid.attemptId,
  });

  try {
    assert.deepEqual(
      Buffer.from(await retained.body.readAll({ maxBytes: binary.length })),
      binary,
    );
  } finally {
    await retained.body.close();
  }

  assert.deepEqual(await app.finalize(OWNER, issued.attemptId), attached);

  await store.putPrincipalPolicy({
    principalId: backend.principalId,
    document: createBackendPolicy(bucket, "private/"),
  });

  assert.deepEqual(await app.cleanup(invalid.expiresAt + 60_000), {
    deleted: 1,
    failed: 0,
  });

  // A still-valid URL can recreate a deleted abandoned key; another pass finds it.
  const replay = await fetch(invalid.upload.url, {
    method: "PUT",
    headers: invalid.upload.headers,
    body: binary,
  });

  await replay.arrayBuffer();
  assert.equal(replay.status, 200);

  await assert.rejects(
    app.finalize(OWNER, invalid.attemptId),
    expectAppError("notFound"),
  );

  assert.deepEqual(await app.cleanup(invalid.expiresAt + 120_000), {
    deleted: 1,
    failed: 0,
  });

  assert.deepEqual(await app.finalize(OWNER, issued.attemptId), attached);
});
