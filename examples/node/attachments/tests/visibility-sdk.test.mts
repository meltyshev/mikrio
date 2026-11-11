import assert from "node:assert/strict";
import { join, dirname } from "node:path";
import { test } from "node:test";
import { Store } from "mikrio";
import {
  S3Client,
  PutObjectTaggingCommand,
  PutObjectCommand,
  DeleteObjectCommand,
  CreateMultipartUploadCommand,
  CopyObjectCommand,
  S3ServiceException,
} from "@aws-sdk/client-s3";
import { Attachments } from "../src/attachments.mjs";
import { createBackendPolicy } from "../src/local.mjs";
import { S3Storage } from "../src/storage.mjs";
import { Visibility } from "../src/visibility.mjs";
import { State } from "../src/state.mjs";
import { CLEANUP_RETRY_MS } from "../src/model.mjs";
import { BYTES, INPUT, OWNER, createFixture } from "./support.mjs";

await test("SDK visibility publication denies forged tags and restores private origin access without moving bytes", async (t) => {
  const { state, options, states, resources } = await createFixture(t);

  const store = await Store.open({
    directory: join(dirname(options.path), "store"),
  });

  resources.defer(() => store.close());
  const bucket = options.bucket;
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
        Resource: `arn:aws:s3:::${bucket}/${options.prefix}*`,
        Condition: { StringEquals: { "s3:if-none-match": "*" } },
      },
    }),
  });

  await store.putPrincipalPolicy({
    principalId: backend.principalId,
    document: createBackendPolicy(bucket, options.prefix, true),
  });

  await store.putPublicAccessBlock({
    bucket,
    configuration: {
      blockPublicAcls: true,
      ignorePublicAcls: true,
      blockPublicPolicy: false,
      restrictPublicBuckets: false,
    },
  });

  await store.putBucketPolicy({
    bucket,
    document: JSON.stringify({
      Version: "2012-10-17",
      Statement: {
        Effect: "Allow",
        Principal: "*",
        Action: "s3:GetObject",
        Resource: `arn:aws:s3:::${bucket}/${options.prefix}*`,
        Condition: {
          StringEquals: { "s3:ExistingObjectTag/visibility": "public" },
        },
      },
    }),
  });

  const uploadKey = await store.createAccessKey({
    principalId: uploader.principalId,
  });

  const backendKey = await store.createAccessKey({
    principalId: backend.principalId,
  });

  const listener = await store.listen({ host: "127.0.0.1", port: 0 });
  resources.defer(() => listener.close());

  const storageOptions = {
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

  const storage = new S3Storage(storageOptions);
  resources.defer(() => storage.close());

  const client = new S3Client({
    endpoint: listener.endpoint,
    region: listener.region,
    forcePathStyle: true,
    credentials: storageOptions.uploader,
    maxAttempts: 1,
  });

  resources.defer(() => client.destroy());
  const app = new Attachments(state, storage, bucket, options.prefix, true);
  const issued = await app.create(OWNER, INPUT);
  const key = options.prefix + issued.attemptId;

  const uploaded = await fetch(issued.upload.url, {
    method: "PUT",
    headers: issued.upload.headers,
    body: BYTES,
  });

  assert.equal(uploaded.status, 200);
  await uploaded.arrayBuffer();
  await app.finalize(OWNER, issued.attemptId);
  const url = listener.endpoint + "/" + bucket + "/" + key;
  const privateRead = await fetch(url);
  assert.equal(privateRead.status, 403);
  await privateRead.arrayBuffer();
  const verified = await store.headObject({ bucket, key });

  // Equal bytes/ETags cannot authorize replacement. Both the app's outstanding
  // URL and the signer identity remain create-only, with no deletion grant.
  await assert.rejects(
    client.send(
      new PutObjectCommand({
        Bucket: bucket,
        Key: key,
        Body: BYTES,
        IfNoneMatch: "*",
      }),
    ),
    (error: unknown) =>
      error instanceof S3ServiceException &&
      error.$metadata.httpStatusCode === 412,
  );

  await assert.rejects(
    client.send(new DeleteObjectCommand({ Bucket: bucket, Key: key })),
    (error: unknown) =>
      error instanceof S3ServiceException &&
      error.$metadata.httpStatusCode === 403,
  );

  const repeated = await fetch(issued.upload.url, {
    method: "PUT",
    headers: issued.upload.headers,
    body: BYTES,
  });

  assert.equal(repeated.status, 412);
  await repeated.arrayBuffer();
  const unchanged = await store.headObject({ bucket, key });
  assert.equal(unchanged.etag, verified.etag);

  assert.equal(
    unchanged.lastModified.getTime(),
    verified.lastModified.getTime(),
  );

  // Reopening the accepted app state retains the live reference and its intent.
  await state.close();
  const reopened = await State.open(options);
  states.push(reopened);

  const visibility = new Visibility(
    reopened,
    storage,
    bucket,
    options.prefix,
    true,
    true,
  );

  await visibility.change(OWNER, issued.attemptId, { visibility: "public" });
  assert.deepEqual(await visibility.reconcile(), { applied: 1, failed: 0 });
  const publicRead = await fetch(url);
  assert.equal(publicRead.status, 200);
  assert.deepEqual(Buffer.from(await publicRead.arrayBuffer()), BYTES);

  const isForbidden = (error: unknown): boolean =>
    error instanceof S3ServiceException &&
    error.$metadata.httpStatusCode === 403;

  await assert.rejects(
    client.send(
      new PutObjectTaggingCommand({
        Bucket: bucket,
        Key: key,
        Tagging: { TagSet: [{ Key: "visibility", Value: "public" }] },
      }),
    ),
    isForbidden,
  );

  await assert.rejects(
    client.send(
      new PutObjectCommand({
        Bucket: bucket,
        Key: options.prefix + "forged-put",
        Body: BYTES,
        IfNoneMatch: "*",
        Tagging: "visibility=public",
      }),
    ),
    isForbidden,
  );

  await assert.rejects(
    client.send(
      new CreateMultipartUploadCommand({
        Bucket: bucket,
        Key: options.prefix + "forged-multipart",
        Tagging: "visibility=public",
      }),
    ),
    isForbidden,
  );

  await assert.rejects(
    client.send(
      new CopyObjectCommand({
        Bucket: bucket,
        Key: options.prefix + "forged-copy",
        CopySource: bucket + "/" + key,
      }),
    ),
    isForbidden,
  );

  await visibility.change(OWNER, issued.attemptId, { visibility: "private" });
  assert.deepEqual(await visibility.reconcile(), { applied: 1, failed: 0 });
  const unpublish = await fetch(url);
  assert.equal(unpublish.status, 403);
  await unpublish.arrayBuffer();
  const object = await store.getObject({ bucket, key });

  try {
    assert.deepEqual(
      Buffer.from(await object.body.readAll({ maxBytes: BYTES.length })),
      BYTES,
    );
  } finally {
    await object.body.close();
  }

  assert.equal((await store.listObjectsV2({ bucket })).contents.length, 1);

  // Deliberately violate the documented operator boundary with trusted native
  // deletion. Missing bytes are reported as failed publication. A still-valid
  // URL can recreate a deleted key with the same ETag: tagging is not identity
  // CAS, so the application must prohibit this deletion/key reuse in production.
  await store.deleteObject({ bucket, key });
  await visibility.change(OWNER, issued.attemptId, { visibility: "public" });
  assert.deepEqual(await visibility.reconcile(), { applied: 0, failed: 1 });

  assert.equal(
    (
      await reopened.call("attachment", {
        accountId: OWNER,
        attemptId: issued.attemptId,
      })
    )?.publication,
    "failed",
  );

  const recreated = await fetch(issued.upload.url, {
    method: "PUT",
    headers: issued.upload.headers,
    body: BYTES,
  });

  assert.equal(recreated.status, 200);
  await recreated.arrayBuffer();
  assert.equal((await store.headObject({ bucket, key })).etag, verified.etag);

  assert.deepEqual(await visibility.reconcile(Date.now() + CLEANUP_RETRY_MS), {
    applied: 1,
    failed: 0,
  });

  const replacedRead = await fetch(url);
  assert.equal(replacedRead.status, 200);
  assert.deepEqual(Buffer.from(await replacedRead.arrayBuffer()), BYTES);
});
