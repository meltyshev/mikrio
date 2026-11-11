import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import {
  S3Client,
  GetObjectCommand,
  S3ServiceException,
} from "@aws-sdk/client-s3";
import { Store, StorageError } from "mikrio";
import { Attachments } from "../src/attachments.mjs";
import { loadConfig } from "../src/config.mjs";
import { State } from "../src/state.mjs";
import { S3Storage } from "../src/storage.mjs";
import { Visibility } from "../src/visibility.mjs";
import { BYTES, OWNER, expectAppError } from "./support.mjs";

interface Expected {
  storeId: string;
  attached: string;
  pending: string;
  abandoned: string;
}

async function restore(): Promise<void> {
  const [directory, expectedPath, mode] = process.argv.slice(2);
  assert.ok(directory && expectedPath);

  assert.ok(
    mode === "matched" || mode === "missing-object" || mode === "lost-history",
  );

  const expected = JSON.parse(await readFile(expectedPath, "utf8")) as Expected;
  const path = join(directory, "config.json");
  const config = await loadConfig(path);
  assert.ok(config.local);
  // Change locations only in this verified working copy. The preserved private
  // configuration and all backup files remain unopened by the engine.
  config.local.directory = join(directory, "store");
  config.state.path = join(directory, "app.sqlite3");
  await using resources = new AsyncDisposableStack();
  const store = await Store.open({ directory: config.local.directory });
  resources.defer(() => store.close());
  assert.equal(store.info.storeId, expected.storeId);
  assert.equal(store.info.formatVersion, 1);
  const state = await State.open(config.state);
  resources.defer(() => state.close());
  const { bucket, prefix } = config.state;
  const location = { bucket, key: prefix + expected.attached };

  const attached = await state.call("attachment", {
    accountId: OWNER,
    attemptId: expected.attached,
  });

  // An accepted flag is an operator assertion, not a recovery-point detector.
  // Use the preserved independent inventory to classify both mismatch orders.
  if (mode === "lost-history") {
    assert.equal(attached, undefined);
  } else {
    assert.ok(attached);
  }

  if (mode === "missing-object") {
    await assert.rejects(
      store.getObject(location),
      (error: unknown) =>
        error instanceof StorageError && error.code === "NoSuchKey",
    );
  } else {
    const object = await store.getObject(location);

    try {
      assert.deepEqual(
        Buffer.from(await object.body.readAll({ maxBytes: BYTES.length })),
        BYTES,
      );
    } finally {
      await object.body.close();
    }
  }

  // Restore revives historical keys. Remove them through trusted administration
  // and review public access before starting even the isolated test listener.
  const backend = (await store.listPrincipals({})).principals.find(
    (principal) => principal.name === "attachment-backend",
  );

  const uploader = (await store.listPrincipals({})).principals.find(
    (principal) => principal.name === "attachment-uploader",
  );

  assert.ok(backend && uploader);
  const historical = config.storage.backend;

  for (const [principal, credentials] of [
    [backend, config.storage.backend],
    [uploader, config.storage.uploader],
  ] as const) {
    assert.ok(
      (
        await store.listAccessKeys({ principalId: principal.principalId })
      ).accessKeys.some((key) => key.accessKeyId === credentials.accessKeyId),
    );

    await store.deleteAccessKey({
      principalId: principal.principalId,
      accessKeyId: credentials.accessKeyId,
    });
  }

  const replacement = await store.createAccessKey({
    principalId: backend.principalId,
  });

  const uploadReplacement = await store.createAccessKey({
    principalId: uploader.principalId,
  });

  config.storage.backend = {
    accessKeyId: replacement.accessKey.accessKeyId,
    secretAccessKey: replacement.secretAccessKey,
  };

  config.storage.uploader = {
    accessKeyId: uploadReplacement.accessKey.accessKeyId,
    secretAccessKey: uploadReplacement.secretAccessKey,
  };

  const historicalPublic = await store.getPublicAccessBlock({ bucket });
  assert.equal(historicalPublic.configuration.blockPublicPolicy, false);

  await store.putPublicAccessBlock({
    bucket,
    configuration: {
      blockPublicAcls: true,
      ignorePublicAcls: true,
      blockPublicPolicy: true,
      restrictPublicBuckets: true,
    },
  });

  const listener = await store.listen({ host: "127.0.0.1", port: 0 });
  resources.defer(() => listener.close());
  config.storage.endpoint = listener.endpoint;

  await writeFile(path, JSON.stringify(config, null, 2) + "\n", {
    mode: 0o600,
  });

  const storage = new S3Storage(config.storage);
  resources.defer(() => storage.close());

  const oldClient = new S3Client({
    endpoint: listener.endpoint,
    region: listener.region,
    credentials: historical,
    forcePathStyle: true,
    maxAttempts: 1,
  });

  resources.defer(() => oldClient.destroy());

  await assert.rejects(
    oldClient.send(new GetObjectCommand({ Bucket: bucket, Key: location.key })),
    (error: unknown) =>
      error instanceof S3ServiceException &&
      error.$metadata.httpStatusCode === 403,
  );

  const paused = new Attachments(state, storage, bucket, prefix, false);

  const pausedVisibility = new Visibility(
    state,
    storage,
    bucket,
    prefix,
    true,
    false,
  );

  const beforeCleanup = await store.listObjectsV2({ bucket, prefix });
  await assert.rejects(paused.cleanup(), expectAppError("unavailable"));

  await assert.rejects(
    pausedVisibility.reconcile(),
    expectAppError("unavailable"),
  );

  assert.deepEqual(
    await store.listObjectsV2({ bucket, prefix }),
    beforeCleanup,
  );

  if (mode !== "matched") {
    assert.deepEqual(
      await state.call("attachment", {
        accountId: OWNER,
        attemptId: expected.attached,
      }),
      attached,
    );

    process.stdout.write(
      JSON.stringify({ classification: mode, accepted: false }) + "\n",
    );

    return;
  }

  assert.equal(
    (
      await state.call("attempt", {
        accountId: OWNER,
        attemptId: expected.pending,
      })
    ).status,
    "pending",
  );

  assert.equal(
    (
      await state.call("attempt", {
        accountId: OWNER,
        attemptId: expected.abandoned,
      })
    ).status,
    "abandoned",
  );

  assert.deepEqual((await store.getObjectTagging(location)).tags, [
    { key: "visibility", value: "public" },
  ]);

  assert.equal(attached?.visibility, "private");
  const visibility = new Visibility(state, storage, bucket, prefix, true, true);
  assert.deepEqual(await visibility.reconcile(), { applied: 1, failed: 0 });

  assert.deepEqual((await store.getObjectTagging(location)).tags, [
    { key: "visibility", value: "private" },
  ]);

  const app = new Attachments(state, storage, bucket, prefix, true);
  assert.deepEqual(await app.cleanup(), { deleted: 1, failed: 0 });

  await assert.rejects(
    state.call("attempt", { accountId: OWNER, attemptId: expected.abandoned }),
    expectAppError("notFound"),
  );

  // A write arriving after terminal-history compaction still has no attachment
  // authority. Restarted prefix reconciliation must discover and reclaim it.
  await store.putObject({
    bucket,
    key: prefix + expected.abandoned,
    body: BYTES,
  });

  const restarted = new Attachments(state, storage, bucket, prefix, true);
  assert.deepEqual(await restarted.cleanup(), { deleted: 1, failed: 0 });

  assert.deepEqual(
    (await storage.list(bucket, prefix)).keys.toSorted(),
    [expected.attached, expected.pending]
      .map((attemptId) => prefix + attemptId)
      .sort(),
  );

  const response = await fetch(await app.download(OWNER, expected.attached), {
    signal: AbortSignal.timeout(10_000),
  });

  assert.equal(response.status, 200);
  assert.deepEqual(Buffer.from(await response.arrayBuffer()), BYTES);

  process.stdout.write(
    JSON.stringify({ classification: "matched", accepted: true }) + "\n",
  );
}

await restore();
