import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { Readable } from "node:stream";
import test from "node:test";
import { Upload } from "@aws-sdk/lib-storage";
import {
  AbortMultipartUploadCommand,
  DeleteObjectCommand,
  GetObjectCommand,
  ListMultipartUploadsCommand,
  ListPartsCommand,
  PutObjectCommand,
} from "@aws-sdk/client-s3";
import { client } from "./http-client.mjs";

const PART_BYTES = 5 * 1024 * 1024;
const TARGET = { Bucket: "photos", Key: "node-transfer" };

function barrier() {
  let resolve = () => {};
  const promise = new Promise<void>((release) => {
    resolve = release;
  });
  return { promise, resolve };
}

function fixture(name: string) {
  const s3 = client();
  const peer = client(process.env["mikrio_fixture_peer"]);
  const calls: string[] = [];
  const events: { operation: string; stage: string; status?: number }[] = [];
  const cleaned = barrier();

  s3.middlewareStack.add(
    (next, context) => async (arguments_) => {
      assert.ok(context.commandName);
      const operation = context.commandName.replace(/Command$/, "");
      calls.push(operation);
      events.push({ operation, stage: "issued" });

      try {
        const result = await next(arguments_);
        const status = result.output.$metadata.httpStatusCode;
        events.push({
          operation,
          stage: "fulfilled",
          ...(status === undefined ? {} : { status }),
        });

        if (context.commandName === "AbortMultipartUploadCommand") {
          cleaned.resolve();
        }

        return result;
      } catch (error) {
        events.push({ operation, stage: "rejected" });
        throw error;
      }
    },
    { step: "initialize", name: "liveTransferObservation" },
  );

  async function readback(expected: Buffer) {
    const response = await peer.send(
      new GetObjectCommand({ ...TARGET, ChecksumMode: "ENABLED" }),
    );
    assert.ok(response.Body);
    assert.deepEqual(
      Buffer.from(await response.Body.transformToByteArray()),
      expected,
    );
    assert.ok(response.ChecksumCRC32);
  }

  async function noParts() {
    const result = await peer.send(
      new ListMultipartUploadsCommand({ Bucket: TARGET.Bucket }),
    );
    assert.deepEqual(result.Uploads ?? [], []);
  }

  async function close() {
    try {
      await peer.send(new DeleteObjectCommand(TARGET));
      const evidence = process.env["mikrio_sdk_live_record"];

      if (evidence) {
        await writeFile(
          join(evidence, `node-${name}.json`),
          JSON.stringify(
            { origin: "live HTTP client calls", calls, events },
            null,
            2,
          ) + "\n",
        );
      }
    } finally {
      s3.destroy();
      peer.destroy();
    }
  }

  return {
    s3,
    peer,
    calls,
    cleaned: cleaned.promise,
    readback,
    noParts,
    [Symbol.asyncDispose]: close,
  };
}

await test("installed native seed and managed clients retain independent readback objects", async () => {
  await using current = fixture("seed");
  const { s3 } = current;

  await s3.send(
    new PutObjectCommand({
      Bucket: "photos",
      Key: "node-shared",
      Body: Buffer.from(
        Array.from({ length: 263 * 1024 }, (_, index) => index % 239),
      ),
    }),
  );

  for (const [key, expected] of [
    ["native-seed", Buffer.from("native-to-sdk")],
    [
      "boto-shared",
      Buffer.from(
        Array.from({ length: 257 * 1024 }, (_, index) => index % 251),
      ),
    ],
  ] as const) {
    const result = await s3.send(
      new GetObjectCommand({ Bucket: "photos", Key: key }),
    );
    assert.ok(result.Body);
    assert.deepEqual(
      Buffer.from(await result.Body.transformToByteArray()),
      expected,
    );
  }
});

for (const source of ["Buffer", "Readable", "ReadableStream"] as const) {
  for (const [delta, boundary] of [
    [-1, "below"],
    [0, "at"],
    [1, "above"],
  ] as const) {
    await test(
      `live ${source} ${boundary} threshold preserves bytes and checksums`,
      { timeout: 30_000 },
      async () => {
        await using current = fixture(`${source}-${boundary}`);

        const bytes = Buffer.alloc(PART_BYTES + delta, "x");
        bytes[bytes.length - 1] = 122;

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
          client: current.s3,
          params: { ...TARGET, Body: body },
          queueSize: 1,
          partSize: PART_BYTES,
        }).done();
        assert.deepEqual(
          current.calls,
          delta <= 0
            ? ["PutObject"]
            : [
                "CreateMultipartUpload",
                "UploadPart",
                "UploadPart",
                "CompleteMultipartUpload",
              ],
        );
        await current.readback(bytes);
        await current.noParts();
      },
    );
  }
}

for (const leavePartsOnError of [false, true]) {
  await test(
    `live producer failure preserves predecessor with leavePartsOnError=${String(leavePartsOnError)}`,
    { timeout: 30_000 },
    async () => {
      await using current = fixture(`producer-${String(leavePartsOnError)}`);
      const acknowledged = barrier();
      const failure = new Error(
        "owned producer failed after acknowledged part",
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
        await current.peer.send(
          new PutObjectCommand({ ...TARGET, Body: "predecessor" }),
        );
        const upload = new Upload({
          client: current.s3,
          params: { ...TARGET, Body: body },
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
        assert.deepEqual(current.calls, [
          "CreateMultipartUpload",
          "UploadPart",
          ...(leavePartsOnError ? [] : ["AbortMultipartUpload"]),
        ]);
        await current.readback(Buffer.from("predecessor"));

        if (leavePartsOnError) {
          const uploads = await current.peer.send(
            new ListMultipartUploadsCommand({ Bucket: TARGET.Bucket }),
          );
          assert.equal(uploads.Uploads?.length, 1);
          const uploadId = uploads.Uploads?.[0]?.UploadId;
          assert.ok(uploadId);
          const parts = await current.peer.send(
            new ListPartsCommand({ ...TARGET, UploadId: uploadId }),
          );
          assert.equal(parts.Parts?.length, 1);
          assert.equal(parts.Parts[0]?.Size, PART_BYTES);
          await current.peer.send(
            new AbortMultipartUploadCommand({ ...TARGET, UploadId: uploadId }),
          );
        }

        await current.noParts();
      } finally {
        acknowledged.resolve();
        body.destroy();
      }
    },
  );
}

await test(
  "live cancellation after acknowledged part waits for abort cleanup and preserves predecessor",
  { timeout: 30_000 },
  async () => {
    await using current = fixture("cancel");

    await current.peer.send(
      new PutObjectCommand({ ...TARGET, Body: "predecessor" }),
    );
    const upload = new Upload({
      client: current.s3,
      params: { ...TARGET, Body: Buffer.alloc(PART_BYTES + 1, "x") },
      partSize: PART_BYTES,
      queueSize: 1,
    });
    let abort: Promise<void> | undefined;
    upload.on("httpUploadProgress", () => {
      abort ??= upload.abort();
    });
    await assert.rejects(upload.done(), { name: "AbortError" });
    assert.ok(abort);
    await abort;
    await current.cleaned;
    assert.deepEqual(current.calls, [
      "CreateMultipartUpload",
      "UploadPart",
      "AbortMultipartUpload",
    ]);
    await current.readback(Buffer.from("predecessor"));
    await current.noParts();
  },
);

await test(
  "live part rejection aborts the transfer and preserves predecessor",
  { timeout: 30_000 },
  async () => {
    await using current = fixture("part-failure");
    let uploadId: string | undefined;

    current.s3.middlewareStack.add(
      (next, context) => async (arguments_) => {
        const result = await next(arguments_);

        if (context.commandName === "CreateMultipartUploadCommand") {
          assert.ok(
            "UploadId" in result.output &&
              typeof result.output.UploadId === "string",
          );
          uploadId = result.output.UploadId;
        }

        if (context.commandName === "UploadPartCommand") {
          assert.ok(uploadId);
          await current.peer.send(
            new AbortMultipartUploadCommand({ ...TARGET, UploadId: uploadId }),
          );
        }

        return result;
      },
      { step: "initialize", name: "retireAcknowledgedUpload" },
    );

    await current.peer.send(
      new PutObjectCommand({ ...TARGET, Body: "predecessor" }),
    );
    await assert.rejects(
      new Upload({
        client: current.s3,
        params: { ...TARGET, Body: Buffer.alloc(PART_BYTES + 1, "x") },
        queueSize: 1,
        partSize: PART_BYTES,
      }).done(),
      { name: "NoSuchUpload" },
    );
    assert.deepEqual(current.calls, [
      "CreateMultipartUpload",
      "UploadPart",
      "UploadPart",
      "AbortMultipartUpload",
    ]);
    await current.readback(Buffer.from("predecessor"));
    await current.noParts();
  },
);

await test(
  "live queue size bounds part workers and completion follows observed part settlement",
  { timeout: 30_000 },
  async (context) => {
    const current = fixture("concurrency");
    const entered = barrier();
    const release = barrier();
    let active = 0;
    let maximum = 0;
    let uploadId: string | undefined;

    current.s3.middlewareStack.add(
      (next, context) => async (arguments_) => {
        if (context.commandName !== "UploadPartCommand") {
          const result = await next(arguments_);

          if (context.commandName === "CreateMultipartUploadCommand") {
            assert.ok(
              "UploadId" in result.output &&
                typeof result.output.UploadId === "string",
            );
            uploadId = result.output.UploadId;
          }

          return result;
        }

        active += 1;
        maximum = Math.max(maximum, active);

        try {
          const result = await next(arguments_);

          if (
            current.calls.filter((name) => name === "UploadPart").length === 2
          ) {
            entered.resolve();
          }

          await release.promise;
          return result;
        } finally {
          active -= 1;
        }
      },
      { step: "initialize", name: "holdAcknowledgedParts" },
    );

    const content = Buffer.alloc(2 * PART_BYTES + 1, "q");
    const pending = new Upload({
      client: current.s3,
      params: { ...TARGET, Body: content },
      partSize: PART_BYTES,
      queueSize: 2,
    }).done();
    const settlement = Promise.allSettled([pending]);

    context.after(async () => {
      await using cleanup = new AsyncDisposableStack();
      cleanup.use(current);
      release.resolve();
      const [result] = await settlement;
      assert.ok(result);

      if (result.status === "rejected") {
        throw result.reason;
      }
    });

    try {
      await entered.promise;
      assert.ok(uploadId);
      assert.deepEqual(current.calls, [
        "CreateMultipartUpload",
        "UploadPart",
        "UploadPart",
      ]);
      assert.equal(maximum, 2);
      release.resolve();
      await pending;
      assert.equal(active, 0);
      assert.equal(maximum, 2);
      assert.deepEqual(current.calls, [
        "CreateMultipartUpload",
        "UploadPart",
        "UploadPart",
        "UploadPart",
        "CompleteMultipartUpload",
      ]);
      await current.readback(content);
      await current.noParts();
    } finally {
      release.resolve();
    }
  },
);
