import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { readdirSync } from "node:fs";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Store, StorageError } from "@mikrio/node";
import { Attachments } from "../src/attachments.mjs";
import { createBackendPolicy } from "../src/local.mjs";
import { State } from "../src/state.mjs";
import { S3Storage } from "../src/storage.mjs";
import { OWNER, expectAppError, createFixture } from "./support.mjs";

await test("attachment fixture closes dependent resources before its state and directory", async (t) => {
  const { state, options, resources } = await createFixture(t);
  let dependencyClosed = false;

  resources.defer(async () => {
    assert.ok((await stat(options.path)).isFile());

    assert.deepEqual(await state.call("attachments", { accountId: OWNER }), []);
    dependencyClosed = true;
  });

  t.after(async () => {
    assert.equal(dependencyClosed, true);
    await assert.rejects(stat(options.path), { code: "ENOENT" });
  });
});

function summarizeDurations(values: number[]): Record<string, number> {
  assert.ok(values.length > 0);
  const ordered = values.toSorted((left, right) => left - right);

  const select = (fraction: number): number => {
    const value = ordered[Math.ceil(fraction * ordered.length) - 1];
    assert.ok(value !== undefined);
    return value;
  };

  return {
    count: values.length,
    p50Ms: select(0.5),
    p95Ms: select(0.95),
    maxMs: select(1),
  };
}

for (const concurrency of [1, 8, 32]) {
  await test(`attachment workload with ${concurrency} clients preserves attached bytes during cleanup`, async (t) => {
    const directory = await mkdtemp(
      join(tmpdir(), "mikrio-attachment-resources-"),
    );

    const resources = new AsyncDisposableStack();

    t.after(async () => {
      let settled = false;

      try {
        await resources.disposeAsync();
        settled = true;
      } finally {
        if (t.passed && settled) {
          await rm(directory, { recursive: true });
        } else {
          t.diagnostic(`retained resource workload: ${directory}`);
        }
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

    await store.putPrincipalPolicy({
      principalId: backend.principalId,
      document: createBackendPolicy(bucket, "private/"),
    });

    const uploadKey = await store.createAccessKey({
      principalId: uploader.principalId,
    });

    const backendKey = await store.createAccessKey({
      principalId: backend.principalId,
    });

    const listener = await store.listen();
    resources.defer(() => listener.close());

    const storage = new S3Storage({
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
    });

    resources.defer(() => storage.close());

    const stateOptions = {
      path: join(directory, "app.sqlite3"),
      bucket,
      prefix: "private/",
      storageId: randomUUID(),
    };

    const state = await State.open({ ...stateOptions, initialize: true });
    resources.defer(() => state.close());
    const accountId = randomUUID();

    await state.call("addAccount", {
      accountId,
      username: "owner",
      salt: "00".repeat(16),
      passwordHash: "00".repeat(32),
    });

    const accounts = Array.from({ length: concurrency }, () => randomUUID());

    for (const [index, clientAccountId] of accounts.entries()) {
      await state.call("addAccount", {
        accountId: clientAccountId,
        username: `client-${index}`,
        salt: "00".repeat(16),
        passwordHash: "00".repeat(32),
      });
    }

    const app = new Attachments(state, storage, bucket, "private/", true);
    const bytes = Buffer.alloc(1024 * 1024, "x");
    const sha256 = createHash("sha256").update(bytes).digest("hex");
    const expired: string[] = [];
    const now = Date.now();

    for (let index = 0; index < 16; index += 1) {
      const attemptId = randomUUID();
      const key = "private/" + attemptId;

      await state.call("createAttempt", {
        attemptId,
        accountId,
        bucket,
        key,
        status: "pending",
        filename: "expired.txt",
        contentLength: bytes.length,
        sha256,
        createdAt: now - 600_000,
        expiresAt: now - 300_000,
      });

      await store.putObject({ bucket, key, body: bytes });
      expired.push(attemptId);
    }

    const baselineMemory = process.memoryUsage();
    const baselineDescriptors = readdirSync("/dev/fd").length;
    let sampledRssBytes = baselineMemory.rss;
    let sampledHeapBytes = baselineMemory.heapUsed;
    let sampledDescriptors = baselineDescriptors;
    let samples = 0;
    let samplesWithCleanupAndClients = 0;
    let cleanupActive = true;

    const sample = (): void => {
      const memory = process.memoryUsage();
      sampledRssBytes = Math.max(sampledRssBytes, memory.rss);
      sampledHeapBytes = Math.max(sampledHeapBytes, memory.heapUsed);

      sampledDescriptors = Math.max(
        sampledDescriptors,
        readdirSync("/dev/fd").length,
      );

      samples += 1;

      if (cleanupActive && active > 0) {
        samplesWithCleanupAndClients += 1;
      }
    };

    const sampler = setInterval(sample, 5);
    resources.defer(() => clearInterval(sampler));
    const createMs: number[] = [];
    const uploadMs: number[] = [];
    const finalizeMs: number[] = [];
    const attached: { accountId: string; attemptId: string }[] = [];
    let active = 0;
    let peakActive = 0;
    let next = 0;
    const failures: unknown[] = [];
    const started = performance.now();
    const cleanupStarted = performance.now();
    const cleanup = app.cleanup();
    assert.equal(app.cleanup(), cleanup);

    const cleanupResult = cleanup.then((result) => {
      cleanupActive = false;
      return { ...result, elapsedMs: performance.now() - cleanupStarted };
    });

    const runClient = async (clientIndex: number): Promise<void> => {
      const clientAccountId = accounts[clientIndex];
      assert.ok(clientAccountId);

      while (next < 32) {
        next += 1;
        active += 1;
        peakActive = Math.max(peakActive, active);

        try {
          let phaseStarted = performance.now();

          const issued = await app.create(clientAccountId, {
            filename: "resource.txt",
            sha256,
            contentLength: bytes.length,
          });

          createMs.push(performance.now() - phaseStarted);
          phaseStarted = performance.now();

          const uploaded = await fetch(issued.upload.url, {
            method: "PUT",
            headers: issued.upload.headers,
            body: bytes,
          });

          await uploaded.arrayBuffer();
          assert.equal(uploaded.status, 200);
          uploadMs.push(performance.now() - phaseStarted);
          phaseStarted = performance.now();
          await app.finalize(clientAccountId, issued.attemptId);
          finalizeMs.push(performance.now() - phaseStarted);

          attached.push({
            accountId: clientAccountId,
            attemptId: issued.attemptId,
          });
        } finally {
          active -= 1;
        }
      }
    };

    // Settle every offered client before closing shared resources after a failure.
    const settled = await Promise.allSettled([
      cleanupResult,
      ...Array.from({ length: concurrency }, (_, index) => runClient(index)),
    ]);

    clearInterval(sampler);
    sample();

    for (const result of settled) {
      if (result.status === "rejected") {
        t.diagnostic(String(result.reason));
        failures.push(result.reason);
      }
    }

    if (failures.length > 0) {
      throw new AggregateError(failures, "Attachment resource workload failed");
    }

    const cleaned = await cleanupResult;
    assert.equal(cleaned.deleted, 16);
    assert.equal(cleaned.failed, 0);
    assert.equal(attached.length, 32);
    assert.equal(peakActive, concurrency);
    assert.equal(active, 0);
    assert.ok(samplesWithCleanupAndClients > 0);
    const phaseElapsedMs = performance.now() - started;

    // The concurrent first page may finish before all clients publish. Once
    // settled, two bounded pages traverse all 32 live objects and compact the
    // terminal batch without deleting any attached bytes.
    for (let page = 0; page < 2; page += 1) {
      assert.deepEqual(await app.cleanup(), { deleted: 0, failed: 0 });
    }

    await state.close();
    const reopened = await State.open(stateOptions);
    resources.defer(() => reopened.close());
    let attachmentCount = 0;

    for (const clientAccountId of accounts) {
      attachmentCount += (
        await reopened.call("attachments", { accountId: clientAccountId })
      ).length;
    }

    assert.equal(attachmentCount, 32);

    for (const { accountId: clientAccountId, attemptId } of attached) {
      assert.equal(
        (
          await reopened.call("attempt", {
            accountId: clientAccountId,
            attemptId,
          })
        ).status,
        "attached",
      );

      const output = await store.getObject({
        bucket,
        key: "private/" + attemptId,
      });

      await using body = output.body;

      assert.deepEqual(
        Buffer.from(await body.readAll({ maxBytes: bytes.length })),
        bytes,
      );
    }

    for (const attemptId of expired) {
      await assert.rejects(
        reopened.call("attempt", { attemptId, accountId }),
        expectAppError("notFound"),
      );

      await assert.rejects(
        store.headObject({ bucket, key: "private/" + attemptId }),
        (error: unknown) => {
          assert.ok(error instanceof StorageError);
          assert.equal(error.code, "NoSuchKey");
          return true;
        },
      );
    }

    t.diagnostic(
      JSON.stringify({
        workload: "attachment-cleanup-contention",
        offeredClients: concurrency,
        peakInFlightWorkflows: peakActive,
        attachmentCount: attached.length,
        expiredCount: expired.length,
        payloadBytes: bytes.length,
        phaseElapsedMs,
        create: summarizeDurations(createMs),
        upload: summarizeDurations(uploadMs),
        finalize: summarizeDurations(finalizeMs),
        cleanup: cleaned,
        baselineMemory,
        baselineDescriptors,
        sampledRssBytes,
        sampledHeapBytes,
        sampledDescriptors,
        samples,
        samplesWithCleanupAndClients,
        processLifetimeMaximumRssBytes: process.resourceUsage().maxRSS * 1024,
        scope:
          "One ordinary Node process includes app worker, SDK, listener/native engine and clients. Five-millisecond samples are observed peaks, not hard bounds. Expired app records are seeded explicitly; this does not measure elapsed signature expiry. No browser, proxy, native allocator or SQL timing claim.",
      }),
    );
  });
}
