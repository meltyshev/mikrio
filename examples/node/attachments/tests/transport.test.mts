import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { test } from "node:test";
import { Attachments } from "../src/attachments.mjs";
import { CLEANUP_RETRY_MS } from "../src/model.mjs";
import { State } from "../src/state.mjs";
import { S3Storage } from "../src/storage.mjs";
import {
  BYTES,
  INPUT,
  OWNER,
  expectAppError,
  createFixture,
} from "./support.mjs";

await test("SDK transport failures preserve pending uploads and prefix reconciliation retries late bytes after lost delete replies", async (t) => {
  const { state, states, options, resources } = await createFixture(t);
  const objects = new Map<string, Buffer>();
  const requests: string[] = [];
  let interruptRead = true;
  let loseDeleteReply = true;

  const server = createServer((request, response) => {
    const path = request.url?.split("?")[0] ?? "";
    requests.push(`${request.method} ${path}`);
    request.resume();

    if (request.method === "GET" && request.url?.includes("list-type=2")) {
      const query = new URL(request.url, "http://owned.invalid").searchParams;
      assert.equal(query.get("prefix"), options.prefix);

      const keys = [...objects.keys()].map((key) =>
        key.slice(options.bucket.length + 2),
      );

      response.writeHead(200, { "content-type": "application/xml" });

      response.end(
        `<ListBucketResult><IsTruncated>false</IsTruncated>${keys.map((key) => `<Contents><Key>${key}</Key></Contents>`).join("")}</ListBucketResult>`,
      );

      return;
    }

    if (request.method === "GET") {
      const bytes = objects.get(path);

      if (!bytes) {
        response.writeHead(404, { "content-type": "application/xml" });
        response.end("<Error><Code>NoSuchKey</Code></Error>");
        return;
      }

      response.writeHead(200, {
        "content-type": "text/plain",
        "content-length": bytes.length,
      });

      if (interruptRead) {
        response.write(bytes.subarray(0, 3), () => response.destroy());
        return;
      }

      response.end(bytes);
      return;
    }

    if (request.method === "DELETE") {
      objects.delete(path);

      if (loseDeleteReply) {
        request.socket.destroy();
        return;
      }

      response.writeHead(204);
      response.end();
      return;
    }

    response.writeHead(405);
    response.end();
  });

  resources.defer(async () => {
    if (!server.listening) {
      return;
    }

    const closed = new Promise<void>((resolve, reject) => {
      server.close((error) => {
        if (error) {
          reject(error);
        } else {
          resolve();
        }
      });
    });

    server.closeAllConnections();
    await closed;
  });

  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert.ok(address && typeof address !== "string");

  const storage = new S3Storage({
    endpoint: `http://127.0.0.1:${address.port}`,
    region: "us-east-1",
    uploader: {
      accessKeyId: "owned-uploader",
      secretAccessKey: "owned-secret",
    },
    backend: { accessKeyId: "owned-backend", secretAccessKey: "owned-secret" },
  });

  resources.defer(() => storage.close());

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const issued = await app.create(OWNER, INPUT);
  const path = `/${options.bucket}/${options.prefix}${issued.attemptId}`;
  objects.set(path, BYTES);

  await assert.rejects(app.finalize(OWNER, issued.attemptId), {
    code: "ECONNRESET",
  });

  assert.ok(requests.length >= 1 && requests.length <= 2);
  assert.ok(requests.every((request) => request === `GET ${path}`));

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
  assert.deepEqual(objects.get(path), BYTES);

  interruptRead = false;
  const attached = await app.finalize(OWNER, issued.attemptId);
  const abandoned = await app.create(OWNER, INPUT);
  const abandonedPath = `/${options.bucket}/${options.prefix}${abandoned.attemptId}`;
  objects.set(abandonedPath, BYTES);
  requests.length = 0;

  assert.deepEqual(await app.cleanup(abandoned.expiresAt), {
    deleted: 0,
    failed: 1,
  });

  // A lost reply cannot authorize attachment. Completed prefix traversal permits
  // compaction while a future traversal still discovers a late admitted write.
  assert.deepEqual(requests, [
    `DELETE ${abandonedPath}`,
    `DELETE ${abandonedPath}`,
    `GET /${options.bucket}/`,
  ]);

  assert.equal(objects.has(abandonedPath), false);
  assert.deepEqual(objects.get(path), BYTES);
  await state.close();

  const reopened = await State.open(options);
  states.push(reopened);

  const restarted = new Attachments(
    reopened,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  await assert.rejects(
    restarted.finalize(OWNER, abandoned.attemptId),
    expectAppError("notFound"),
  );

  objects.set(abandonedPath, BYTES);
  loseDeleteReply = false;
  requests.length = 0;

  assert.deepEqual(
    await restarted.cleanup(abandoned.expiresAt + CLEANUP_RETRY_MS),
    {
      deleted: 1,
      failed: 0,
    },
  );

  assert.deepEqual(requests, [
    `GET /${options.bucket}/`,
    `DELETE ${abandonedPath}`,
  ]);

  await assert.rejects(
    reopened.call("attempt", {
      accountId: OWNER,
      attemptId: abandoned.attemptId,
    }),
    expectAppError("notFound"),
  );

  assert.deepEqual(await reopened.call("attachments", { accountId: OWNER }), [
    attached,
  ]);

  assert.deepEqual(objects.get(path), BYTES);
});
