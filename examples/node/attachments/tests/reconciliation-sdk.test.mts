import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  ListBucketsCommand,
  ListObjectsV2Command,
  S3Client,
  S3ServiceException,
} from "@aws-sdk/client-s3";
import { Store } from "mikrio";
import { Attachments } from "../src/attachments.mjs";
import { loadConfig } from "../src/config.mjs";
import {
  configureLocalReconciliation,
  initializeLocal,
} from "../src/local.mjs";
import { State } from "../src/state.mjs";
import { S3Storage } from "../src/storage.mjs";
import { BYTES, INPUT, OWNER, expectAppError } from "./support.mjs";

await test("fresh and existing backend grants permit only the owned listing prefix and real SDK scans preserve current app ownership", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "mikrio-reconciliation-sdk-"));
  const resources = new AsyncDisposableStack();

  t.after(async () => {
    await resources.disposeAsync();

    if (t.passed) {
      await rm(directory, { recursive: true });
    } else {
      t.diagnostic(`retained failed reconciliation SDK state: ${directory}`);
    }
  });

  const config = await loadConfig(
    await initializeLocal(join(directory, "app")),
  );

  assert.ok(config.local);
  const store = await Store.open({ directory: config.local.directory });
  resources.defer(() => store.close());
  const listener = await store.listen({ host: "127.0.0.1", port: 0 });
  resources.defer(() => listener.close());

  const createClient = (credentials: typeof config.storage.backend): S3Client =>
    new S3Client({
      endpoint: listener.endpoint,
      region: listener.region,
      forcePathStyle: true,
      credentials,
      maxAttempts: 1,
    });

  const backend = createClient(config.storage.backend);
  const uploader = createClient(config.storage.uploader);
  resources.defer(() => backend.destroy());
  resources.defer(() => uploader.destroy());
  const { bucket, prefix } = config.state;

  const expectAccessDenied = (error: unknown): boolean => {
    assert.ok(error instanceof S3ServiceException);
    assert.equal(error.name, "AccessDenied");
    assert.equal(error.$metadata.httpStatusCode, 403);
    return true;
  };

  const probe = async (): Promise<void> => {
    assert.equal(
      (
        await backend.send(
          new ListObjectsV2Command({ Bucket: bucket, Prefix: prefix }),
        )
      ).IsTruncated,
      false,
    );

    for (const candidate of [
      undefined,
      "",
      "priv",
      "outside/",
      prefix + "narrower/",
    ]) {
      await assert.rejects(
        backend.send(
          new ListObjectsV2Command({
            Bucket: bucket,
            ...(candidate === undefined ? {} : { Prefix: candidate }),
          }),
        ),
        expectAccessDenied,
      );
    }

    await assert.rejects(
      backend.send(new ListBucketsCommand({})),
      expectAccessDenied,
    );

    await assert.rejects(
      uploader.send(
        new ListObjectsV2Command({ Bucket: bucket, Prefix: prefix }),
      ),
      expectAccessDenied,
    );
  };

  await probe();

  const principals = (await store.listPrincipals({})).principals;

  const principal = principals.find(
    (candidate) => candidate.name === "attachment-backend",
  );

  const uploadPrincipal = principals.find(
    (candidate) => candidate.name === "attachment-uploader",
  );

  assert.ok(principal && uploadPrincipal);

  await store.putPrincipalPolicy({
    principalId: principal.principalId,
    document: JSON.stringify({
      Version: "2012-10-17",
      Statement: {
        Effect: "Allow",
        Action: ["s3:GetObject", "s3:DeleteObject"],
        Resource: `arn:aws:s3:::${bucket}/${prefix}*`,
      },
    }),
  });

  await assert.rejects(
    backend.send(new ListObjectsV2Command({ Bucket: bucket, Prefix: prefix })),
    expectAccessDenied,
  );

  await assert.rejects(
    configureLocalReconciliation(config, uploadPrincipal.principalId),
    expectAppError("invalid"),
  );

  await configureLocalReconciliation(config, principal.principalId);
  await probe();

  const state = await State.open(config.state);
  resources.defer(() => state.close());

  await state.call("addAccount", {
    accountId: OWNER,
    username: OWNER,
    salt: "00".repeat(16),
    passwordHash: "00".repeat(32),
  });

  const storage = new S3Storage({
    ...config.storage,
    endpoint: listener.endpoint,
  });

  resources.defer(() => storage.close());
  const app = new Attachments(state, storage, bucket, prefix, true);
  const issued = await app.create(OWNER, INPUT);
  const pending = await app.create(OWNER, INPUT);

  for (const attempt of [issued, pending]) {
    const response = await fetch(attempt.upload.url, {
      method: "PUT",
      headers: attempt.upload.headers,
      body: BYTES,
    });

    await response.arrayBuffer();
    assert.equal(response.status, 200);
  }

  const attached = await app.finalize(OWNER, issued.attemptId);

  await store.putObject({
    bucket,
    key: prefix + "late-unrecorded",
    body: BYTES,
  });

  assert.deepEqual(await app.cleanup(), { deleted: 1, failed: 0 });

  assert.deepEqual(
    (await storage.list(bucket, prefix)).keys.toSorted(),
    [issued, pending].map((attempt) => prefix + attempt.attemptId).sort(),
  );

  assert.deepEqual(await state.call("attachments", { accountId: OWNER }), [
    attached,
  ]);
});
