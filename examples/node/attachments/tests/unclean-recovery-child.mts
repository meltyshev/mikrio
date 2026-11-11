import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { Store } from "mikrio";
import { Attachments } from "../src/attachments.mjs";
import { loadConfig } from "../src/config.mjs";
import { initializeLocal } from "../src/local.mjs";
import { State } from "../src/state.mjs";
import { S3Storage } from "../src/storage.mjs";
import { BYTES, INPUT, OWNER } from "./support.mjs";

interface Expected {
  storeId: string;
  attemptId: string;
  uploadId: string;
  partEtag: string;
  completedEtag: string;
}

async function run(): Promise<void> {
  const [mode, directory, expectedPath] = process.argv.slice(2);
  assert.ok(directory && expectedPath);
  assert.ok(mode === "write" || mode === "verify");
  const path = join(directory, "config.json");

  if (mode === "write") {
    await initializeLocal(directory);
  }

  const config = await loadConfig(path);
  await using resources = new AsyncDisposableStack();
  const store = await Store.open({ directory: join(directory, "store") });
  resources.defer(() => store.close());

  const state = await State.open({
    ...config.state,
    path: join(directory, "app.sqlite3"),
  });

  resources.defer(() => state.close());
  const bucket = "recovery-layouts";

  if (mode === "write") {
    await state.call("addAccount", {
      accountId: OWNER,
      username: OWNER,
      salt: "00".repeat(16),
      passwordHash: "00".repeat(32),
    });

    const listener = await store.listen({ host: "127.0.0.1", port: 0 });
    resources.defer(() => listener.close());

    const storage = new S3Storage({
      ...config.storage,
      endpoint: listener.endpoint,
    });

    resources.defer(() => storage.close());

    const app = new Attachments(
      state,
      storage,
      config.state.bucket,
      config.state.prefix,
      true,
    );

    const issued = await app.create(OWNER, INPUT);

    const response = await fetch(issued.upload.url, {
      method: "PUT",
      headers: issued.upload.headers,
      body: BYTES,
      signal: AbortSignal.timeout(10_000),
    });

    await response.arrayBuffer();
    assert.equal(response.status, 200);
    await app.finalize(OWNER, issued.attemptId);

    await store.createBucket({ bucket });

    const incomplete = await store.createMultipartUpload({
      bucket,
      key: "pending",
    });

    const part = await store.uploadPart({
      bucket,
      key: "pending",
      uploadId: incomplete.uploadId,
      partNumber: 7,
      body: BYTES,
    });

    const completed = await store.createMultipartUpload({
      bucket,
      key: "completed",
      metadata: { provenance: "before-crash" },
    });

    const completedPart = await store.uploadPart({
      bucket,
      key: "completed",
      uploadId: completed.uploadId,
      partNumber: 3,
      body: BYTES,
    });

    const published = await store.completeMultipartUpload({
      bucket,
      key: "completed",
      uploadId: completed.uploadId,
      parts: [{ partNumber: 3, etag: completedPart.etag }],
    });

    const expected: Expected = {
      storeId: store.info.storeId,
      attemptId: issued.attemptId,
      uploadId: incomplete.uploadId,
      partEtag: part.etag,
      completedEtag: published.etag,
    };

    await writeFile(expectedPath, JSON.stringify(expected), { mode: 0o600 });
    assert.ok(typeof process.send === "function");
    process.send("ready");
    // The parent terminates this process after every publication and the app
    // attachment have acknowledged. Keep both databases open until that kill.
    await new Promise<void>(() => {});
  }

  try {
    const expected = JSON.parse(
      await readFile(expectedPath, "utf8"),
    ) as Expected;

    assert.equal(store.info.storeId, expected.storeId);
    assert.equal(store.info.formatVersion, 1);

    const attachment = await state.call("attachment", {
      accountId: OWNER,
      attemptId: expected.attemptId,
    });

    assert.ok(attachment);

    for (const input of [
      {
        bucket: config.state.bucket,
        key: config.state.prefix + expected.attemptId,
      },
      // Completed reads select assembly positions, not original upload numbers.
      { bucket, key: "completed", partNumber: 1 },
    ]) {
      const object = await store.getObject(input);

      try {
        assert.deepEqual(
          Buffer.from(await object.body.readAll({ maxBytes: BYTES.length })),
          BYTES,
        );
      } finally {
        await object.body.close();
      }
    }

    const head = await store.headObject({ bucket, key: "completed" });
    assert.equal(head.etag, expected.completedEtag);
    assert.equal(head.metadata.provenance, "before-crash");

    const attributes = await store.getObjectAttributes({
      bucket,
      key: "completed",
      objectAttributes: ["ObjectParts"],
    });

    assert.equal(attributes.objectParts?.totalPartsCount, 1);
    // The default FULL_OBJECT checksum projects a count-only Attributes view.
    assert.deepEqual(attributes.objectParts?.parts, []);
    const upload = { bucket, key: "pending", uploadId: expected.uploadId };
    const parts = await store.listParts(upload);
    assert.equal(parts.parts.length, 1);
    assert.equal(parts.parts[0]?.partNumber, 7);
    assert.equal(parts.parts[0]?.etag, expected.partEtag);

    await store.completeMultipartUpload({
      ...upload,
      parts: [{ partNumber: 7, etag: expected.partEtag }],
    });

    const resumed = await store.getObject({ bucket, key: "pending" });

    try {
      assert.deepEqual(
        Buffer.from(await resumed.body.readAll({ maxBytes: BYTES.length })),
        BYTES,
      );
    } finally {
      await resumed.body.close();
    }
  } finally {
    await resources.disposeAsync();
  }

  process.stdout.write("verified\n");
}

await run();
