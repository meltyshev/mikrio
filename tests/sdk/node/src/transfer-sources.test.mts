// Authored transport replies establish helper behavior, not service durability.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { Readable } from "node:stream";
import test from "node:test";
import { S3Client } from "@aws-sdk/client-s3";
import { Upload } from "@aws-sdk/lib-storage";

const PART_BYTES = 5 * 1024 * 1024;

interface Exchange {
  operation: string;
  bytes: number;
  sha256: string;
  checksum?: string | undefined;
}

function barrier() {
  let resolve: () => void = () => {
    throw new Error("barrier not initialized");
  };

  const promise = new Promise<void>((release) => {
    resolve = release;
  });

  return { promise, resolve };
}

function fixture(onPart: () => Promise<void> = () => Promise.resolve()) {
  const exchanges: Exchange[] = [];
  const cleaned = barrier();
  let operation = "";

  const client = new S3Client({
    endpoint: "http://127.0.0.1:1",
    region: "us-east-1",
    forcePathStyle: true,
    maxAttempts: 1,
    credentials: {
      accessKeyId: "synthetic-key",
      secretAccessKey: "synthetic-secret",
    },
    requestHandler: {
      async handle(request: {
        protocol: string;
        hostname: string;
        port?: number;
        headers: Record<string, string>;
        body?: unknown;
      }) {
        assert.equal(request.protocol, "http:");
        assert.equal(request.hostname, "127.0.0.1");
        assert.equal(request.port, 1);
        const input = request.body ?? "";
        assert.ok(typeof input === "string" || input instanceof Uint8Array);
        const body = Buffer.from(input);
        assert.ok(body.length <= PART_BYTES + 1024);
        const checksum = request.headers["x-amz-checksum-crc32"];

        exchanges.push({
          operation,
          bytes: body.length,
          sha256: createHash("sha256").update(body).digest("hex"),
          checksum,
        });

        if (operation === "UploadPart") {
          await onPart();
        }

        const responses: Record<string, string> = {
          CreateMultipartUpload:
            "<InitiateMultipartUploadResult><Bucket>bucket</Bucket><Key>key</Key><UploadId>owned-upload</UploadId></InitiateMultipartUploadResult>",
          CompleteMultipartUpload:
            '<CompleteMultipartUploadResult><Bucket>bucket</Bucket><Key>key</Key><ETag>"fixture-2"</ETag></CompleteMultipartUploadResult>',
        };

        const headers: Record<string, string> = {
          "content-type": "application/xml",
          etag: '"fixture"',
        };

        if (checksum) {
          headers["x-amz-checksum-crc32"] = checksum;
        }

        return {
          response: {
            statusCode: 200,
            headers,
            body: Buffer.from(responses[operation] ?? ""),
          },
        };
      },
    },
  });

  // Initiation precedes every part; completion/cleanup follows the part workers.
  // Concurrent requests in the bounded-concurrency case are all UploadPart.
  client.middlewareStack.add(
    (next, context) => async (arguments_) => {
      assert.ok(context.commandName);
      operation = context.commandName.replace(/Command$/, "");
      const result = await next(arguments_);

      if (context.commandName === "AbortMultipartUploadCommand") {
        cleaned.resolve();
      }

      return result;
    },
    { step: "initialize", name: "ownedTransferObservation" },
  );

  return { client, exchanges, cleaned: cleaned.promise };
}

async function retain(name: string, cases: unknown[]) {
  const destination = process.env["mikrio_sdk_defaults_record"];

  if (!destination) {
    return;
  }

  const path = destination.replace(
    /node-requests\.json$/,
    `node-transfer-${name}.json`,
  );

  assert.notEqual(path, destination);

  await writeFile(
    path,
    JSON.stringify(
      {
        origin: "JavaScript v3; authored responses; no service execution",
        cases,
      },
      null,
      2,
    ) + "\n",
  );
}

await test(
  "parallel parts respect queue size and completion waits for settlement",
  { timeout: 30_000 },
  async (context) => {
    const entered = barrier();
    const release = barrier();
    let activeParts = 0;
    let maxActiveParts = 0;

    const { client, exchanges } = fixture(async () => {
      activeParts += 1;
      maxActiveParts = Math.max(maxActiveParts, activeParts);

      if (activeParts === 2) {
        entered.resolve();
      }

      try {
        await release.promise;
      } finally {
        activeParts -= 1;
      }
    });

    try {
      const pending = new Upload({
        client,
        params: {
          Bucket: "bucket",
          Key: "key",
          Body: Buffer.alloc(2 * PART_BYTES + 1, "x"),
        },
        queueSize: 2,
        partSize: PART_BYTES,
      }).done();

      // Observe rejection immediately and settle the real workers before releasing
      // the client, including when an assertion or the test watchdog fails.
      const settlement = Promise.allSettled([pending]);

      context.after(async () => {
        release.resolve();

        try {
          const [result] = await settlement;
          assert.ok(result);

          if (result.status === "rejected") {
            throw result.reason;
          }
        } finally {
          client.destroy();
        }
      });

      await entered.promise;

      assert.deepEqual(
        exchanges.map((exchange) => exchange.operation),
        ["CreateMultipartUpload", "UploadPart", "UploadPart"],
      );

      release.resolve();
      await pending;
      assert.equal(maxActiveParts, 2);

      assert.deepEqual(
        exchanges.map((exchange) => exchange.operation),
        [
          "CreateMultipartUpload",
          "UploadPart",
          "UploadPart",
          "UploadPart",
          "CompleteMultipartUpload",
        ],
      );

      await retain("concurrency", [
        { maximumActiveParts: maxActiveParts, exchanges },
      ]);
    } finally {
      release.resolve();
    }
  },
);

await test(
  "managed upload preserves threshold edges across buffer and stream sources",
  { timeout: 30_000 },
  async () => {
    const cases = [];

    for (const source of ["Buffer", "Readable", "ReadableStream"] as const) {
      for (const payloadBytes of [PART_BYTES - 1, PART_BYTES, PART_BYTES + 1]) {
        const { client, exchanges } = fixture();

        try {
          const bytes = Buffer.alloc(payloadBytes, "x");

          const body =
            source === "Buffer"
              ? bytes
              : source === "Readable"
                ? Readable.from([bytes])
                : new ReadableStream<Uint8Array>({
                    start(controller) {
                      controller.enqueue(bytes);
                      controller.close();
                    },
                  });

          await new Upload({
            client,
            params: { Bucket: "bucket", Key: "key", Body: body },
            queueSize: 1,
            partSize: PART_BYTES,
          }).done();

          assert.deepEqual(
            exchanges.map((exchange) => exchange.operation),
            payloadBytes <= PART_BYTES
              ? ["PutObject"]
              : [
                  "CreateMultipartUpload",
                  "UploadPart",
                  "UploadPart",
                  "CompleteMultipartUpload",
                ],
            `${source}/${payloadBytes}`,
          );

          const payloads = exchanges.filter((exchange) =>
            ["PutObject", "UploadPart"].includes(exchange.operation),
          );

          assert.equal(
            payloads.reduce(
              (totalBytes, exchange) => totalBytes + exchange.bytes,
              0,
            ),
            payloadBytes,
          );

          assert.ok(payloads.every((exchange) => exchange.checksum));
          cases.push({ source, size: payloadBytes, exchanges });
        } finally {
          client.destroy();
        }
      }
    }

    await retain("sources", cases);
  },
);

await test(
  "producer failure preserves the cause and honors multipart cleanup choice",
  { timeout: 30_000 },
  async () => {
    const cases = [];

    for (const leavePartsOnError of [false, true]) {
      const acknowledged = barrier();
      const { client, exchanges } = fixture();

      const failure = new Error(
        "owned source failed after an acknowledged part",
      );

      let finalized = false;

      async function* source() {
        try {
          yield Buffer.alloc(PART_BYTES + 1, "x");
          await acknowledged.promise;
          throw failure;
        } finally {
          finalized = true;
        }
      }

      const body = Readable.from(source());

      try {
        const upload = new Upload({
          client,
          params: { Bucket: "bucket", Key: "key", Body: body },
          partSize: PART_BYTES,
          queueSize: 1,
          leavePartsOnError,
        });

        upload.on("httpUploadProgress", () => acknowledged.resolve());

        await assert.rejects(
          upload.done(),
          (error: unknown) => error === failure,
        );

        assert.equal(finalized, true);

        assert.deepEqual(
          exchanges.map((exchange) => exchange.operation),
          [
            "CreateMultipartUpload",
            "UploadPart",
            ...(leavePartsOnError ? [] : ["AbortMultipartUpload"]),
          ],
        );

        cases.push({ leavePartsOnError, finalized, exchanges });
      } finally {
        body.destroy();
        client.destroy();
      }
    }

    await retain("failure", cases);
  },
);

await test(
  "abort rejects before the in-flight part settles and cleanup follows settlement",
  { timeout: 30_000 },
  async () => {
    const entered = barrier();
    const release = barrier();

    const { client, exchanges, cleaned } = fixture(() => {
      entered.resolve();
      return release.promise;
    });

    try {
      const upload = new Upload({
        client,
        params: {
          Bucket: "bucket",
          Key: "key",
          Body: Buffer.alloc(PART_BYTES + 1, "x"),
        },
        partSize: PART_BYTES,
        queueSize: 1,
      });

      const done = upload.done();
      await entered.promise;
      await upload.abort();
      await assert.rejects(done, { name: "AbortError" });

      assert.deepEqual(
        exchanges.map((exchange) => exchange.operation),
        ["CreateMultipartUpload", "UploadPart"],
      );

      release.resolve();
      await cleaned;

      assert.deepEqual(
        exchanges.map((exchange) => exchange.operation),
        ["CreateMultipartUpload", "UploadPart", "AbortMultipartUpload"],
      );

      await retain("cancellation", [
        { earlyRejectionBeforeCleanup: true, exchanges },
      ]);
    } finally {
      release.resolve();
      client.destroy();
    }
  },
);
