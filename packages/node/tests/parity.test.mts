import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import { test, type TestContext } from "node:test";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";
import { Store, StorageError, type PutObjectOutput } from "@mikrio/node";

async function fixture(
  context: TestContext,
): Promise<{ store: Store; path: string }> {
  const path = await mkdtemp(join(tmpdir(), "mikrio-node-parity-"));
  let store: Store | undefined = undefined;

  context.after(async () => {
    await store?.close();

    if (context.passed) {
      await rm(path, { recursive: true });
    } else {
      context.diagnostic(`retained failed fixture: ${path}`);
    }
  });

  store = await Store.open({ directory: path });

  await store.createBucket({ bucket: "bucket" });
  return { store, path };
}

function failure(
  expected: string,
  outcome?: string,
): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof StorageError);
    assert.equal(error.code, expected);
    assert.equal(error.mutationOutcome, outcome);
    return true;
  };
}

// These vectors are independently pinned in the shared checksum catalog.
const CHECKSUMS = [
  ["CRC32", "checksumCrc32", "y/Q5Jg=="],
  ["CRC32C", "checksumCrc32c", "4waSgw=="],
  ["SHA1", "checksumSha1", "98O8HYCOBHMq32eZZczDTKeuNEE="],
  ["SHA256", "checksumSha256", "FeKw08M4keuw8e9gnsQZQgwg4yDOlMZfvIwzEkSOsiU="],
  ["CRC64NVME", "checksumCrc64Nvme", "rosUhgp5mIg="],
] as const;

await test("parity: native SQLite coexists in both load orders", async (context) => {
  const { path } = await fixture(context);

  for (const order of ["sqlite-first", "engine-first"]) {
    const output = execFileSync(
      process.execPath,
      [
        fileURLToPath(new URL("coexist.mjs", import.meta.url)),
        order,
        join(path, order),
      ],
      { encoding: "utf8", timeout: 30_000, stdio: ["ignore", "pipe", "pipe"] },
    );

    assert.equal(output, `PASS ${order}\n`);
  }
});

await test("parity: all integrity fields and representation outputs are detached", async (context) => {
  const { store } = await fixture(context);

  for (const [algorithm, field, expected] of CHECKSUMS) {
    const put = await store.putObject({
      bucket: "bucket",
      key: algorithm,
      body: Buffer.from("123456789"),
      contentLength: 9n,
      contentMd5: "JfnnlDI7RTiF9RgfG2JNCw==",
      checksumAlgorithm: algorithm,
      [field]: expected,
      contentType: "application/example",
      contentEncoding: "identity",
      contentDisposition: "inline",
      contentLanguage: "en",
      cacheControl: "max-age=7",
      expires: new Date("0001-01-01T00:00:00.000Z"),
      metadata: { MiXeD: "snow 雪", empty: "" },
      tags: [
        { key: "Case", value: "" },
        { key: "case", value: "one" },
      ],
    });

    for (const [, outputField] of CHECKSUMS) {
      assert.equal(
        put[outputField],
        outputField === field ? expected : undefined,
      );
    }

    const hidden = await store.headObject({ bucket: "bucket", key: algorithm });

    for (const [, outputField] of CHECKSUMS) {
      assert.equal(hidden[outputField], undefined);
    }

    const output = await store.getObject({
      bucket: "bucket",
      key: algorithm,
      checksumMode: "ENABLED",
    });

    await using body = output.body;

    assert.deepEqual(
      new Uint8Array(await body.readAll({ maxBytes: 9 })),
      new Uint8Array(Buffer.from("123456789")),
    );

    await store.deleteObject({ bucket: "bucket", key: algorithm });
    assert.equal(output[field], expected);
    assert.equal(output.checksumType, "FULL_OBJECT");
    assert.deepEqual(output.metadata, { mixed: "snow 雪", empty: "" });
    assert.equal(output.tagCount, 2);
    assert.equal(output.contentType, "application/example");
    assert.equal(output.contentEncoding, "identity");
    assert.equal(output.contentDisposition, "inline");
    assert.equal(output.contentLanguage, "en");
    assert.equal(output.cacheControl, "max-age=7");
    assert.equal(output.expires?.toISOString(), "0001-01-01T00:00:00.000Z");
    assert.equal(output.partsCount, undefined);
    assert.equal(output.contentRange, undefined);
  }
});

await test("parity: empty optional fields and pre-epoch dates survive reopen", async (context) => {
  const { store, path } = await fixture(context);

  await store.putObject({
    bucket: "bucket",
    key: "empty",
    body: new Uint8Array(),
    contentType: "",
    contentEncoding: "",
    contentDisposition: "",
    contentLanguage: "",
    cacheControl: "",
    expires: new Date(-1),
    tags: [],
    metadata: {},
  });

  await store.close();
  await using reopened = await Store.open({ directory: path });
  const output = await reopened.headObject({ bucket: "bucket", key: "empty" });

  for (const field of [
    "contentType",
    "contentEncoding",
    "contentDisposition",
    "contentLanguage",
    "cacheControl",
  ] as const) {
    assert.equal(output[field], "");
  }

  assert.equal(output.expires?.getTime(), -1);
  assert.equal(output.tagCount, 0);
  assert.equal(output.contentLength, 0n);
});

await test("parity: every supplied checksum rejects mismatched bytes before replacement", async (context) => {
  const { store } = await fixture(context);

  await store.putObject({
    bucket: "bucket",
    key: "key",
    body: Buffer.from("predecessor"),
  });

  for (const [, field, value] of CHECKSUMS) {
    // No algorithm selector can mask an ignored named checksum input.
    await assert.rejects(
      store.putObject({
        bucket: "bucket",
        key: "key",
        body: Buffer.from("wrong bytes"),
        [field]: value,
      }),
      failure("BadDigest", "NotCommitted"),
    );
  }

  await using body = (await store.getObject({ bucket: "bucket", key: "key" }))
    .body;

  assert.equal(
    Buffer.from(await body.readAll({ maxBytes: 11 })).toString(),
    "predecessor",
  );
});

await test("parity: malformed values fail before producer pulls and preserve predecessor", async (context) => {
  const { store } = await fixture(context);

  await store.putObject({
    bucket: "bucket",
    key: "key",
    body: Buffer.from("predecessor"),
  });

  // Deliberately widen this call only in the runtime misuse fixture.
  const put = store.putObject.bind(store) as (
    input: unknown,
  ) => Promise<PutObjectOutput>;

  const invalid: [
    string,
    Record<string, unknown>,
    string | typeof TypeError,
  ][] = [
    ["negative length", { contentLength: -1n }, "InvalidArgument"],
    ["overflowing length", { contentLength: 2n ** 64n }, "InvalidArgument"],
    [
      "oversized object",
      { contentLength: 5n * 1024n ** 3n + 1n },
      "InvalidArgument",
    ],
    ["boolean length", { contentLength: true }, TypeError],
    ["fractional length", { contentLength: 1.5 }, TypeError],
    ["NaN length", { contentLength: NaN }, TypeError],
    ["infinite length", { contentLength: Infinity }, TypeError],
    [
      "folded metadata collision",
      { metadata: { A: "a", a: "b" } },
      "InvalidArgument",
    ],
    [
      "metadata surrogate",
      { metadata: { surrogate: "\ud800" } },
      "InvalidArgument",
    ],
    ["numeric metadata value", { metadata: { key: 1 } }, TypeError],
    [
      "Map metadata",
      { metadata: new Map([["key", "must not disappear"]]) },
      TypeError,
    ],
    ["Set metadata", { metadata: new Set(["must not disappear"]) }, TypeError],
    ["Date metadata", { metadata: new Date() }, TypeError],
    ["empty owner", { expectedBucketOwner: "" }, "InvalidArgument"],
    [
      "non-ASCII owner",
      { expectedBucketOwner: "１２３４５６７８９０１２" },
      "InvalidArgument",
    ],
    ["unknown algorithm", { checksumAlgorithm: "sha256" }, "InvalidArgument"],
    ["noncanonical checksum", { checksumCrc32: "AAAAAB==" }, "InvalidArgument"],
    [
      "duplicate tags",
      {
        tags: [
          { key: "same", value: "a" },
          { key: "same", value: "b" },
        ],
      },
      "InvalidArgument",
    ],
    ["invalid date", { expires: new Date(NaN) }, "InvalidArgument"],
    [
      "date before supported range",
      { expires: new Date("0000-12-31T23:59:59.999Z") },
      "InvalidArgument",
    ],
    [
      "date after supported range",
      { expires: new Date("+010000-01-01T00:00:00.000Z") },
      "InvalidArgument",
    ],
    ["null date", { expires: null }, TypeError],
    ["null encoding", { contentEncoding: null }, TypeError],
    ["null tags", { tags: null }, TypeError],
    ["null metadata", { metadata: null }, TypeError],
  ];

  for (const [name, options, expected] of invalid) {
    let pulls = 0;

    async function* source(): AsyncGenerator<Uint8Array> {
      pulls += 1;
      await Promise.resolve();
      yield Buffer.from("unreachable");
    }

    await assert.rejects(
      put({ bucket: "bucket", key: "key", body: source(), ...options }),
      typeof expected === "string"
        ? failure(expected, "NotCommitted")
        : expected,
      name,
    );

    assert.equal(pulls, 0, name);
  }

  await using body = (await store.getObject({ bucket: "bucket", key: "key" }))
    .body;

  assert.equal(
    Buffer.from(await body.readAll({ maxBytes: 11 })).toString(),
    "predecessor",
  );
});

await test("parity: binary Readable and offset Buffer preserve exact bytes", async (context) => {
  const { store } = await fixture(context);
  const original = Buffer.from([99, 99, 0, 255, 1, 2, 3, 99]);

  await store.putObject({
    bucket: "bucket",
    key: "view",
    body: original.subarray(2, 7),
  });

  original.fill(42);

  await using view = (await store.getObject({ bucket: "bucket", key: "view" }))
    .body;

  assert.deepEqual(
    new Uint8Array(await view.readAll({ maxBytes: 5 })),
    Uint8Array.of(0, 255, 1, 2, 3),
  );

  const source = Readable.from([
    Buffer.from(""),
    Buffer.from("binary"),
    Buffer.from(""),
    Buffer.from("-file"),
  ]);

  await store.putObject({ bucket: "bucket", key: "file", body: source });

  await using file = (await store.getObject({ bucket: "bucket", key: "file" }))
    .body;

  assert.equal(
    Buffer.from((await file.readChunk({ maxBytes: 3 })) ?? []).toString(),
    "bin",
  );

  assert.equal(
    Buffer.from(await file.readAll({ maxBytes: 8 })).toString(),
    "ary-file",
  );

  assert.equal(await file.readChunk({ maxBytes: 1 }), null);
  assert.equal(await file.readChunk({ maxBytes: 1 }), null);
});

await test("parity: upload copies actual typed-array views without invoking caller overrides", async (context) => {
  const { store } = await fixture(context);

  const expected = Uint8Array.from(
    { length: 2 * 64 * 1024 + 7 },
    (_, index) => index % 251,
  );

  for (const shared of [false, true]) {
    for (const streamed of [false, true]) {
      for (const overrides of ["subarray", "properties"]) {
        const backing = shared
          ? new SharedArrayBuffer(expected.length + 6)
          : new ArrayBuffer(expected.length + 6);

        const chunk = new Uint8Array(backing, 3, expected.length);
        chunk.set(expected);
        let calls = 0;

        if (overrides === "subarray") {
          Object.defineProperty(chunk, "subarray", {
            value: () => {
              calls += 1;
              return Uint8Array.of(99);
            },
          });
        } else {
          // Typed-array own properties cannot redefine its underlying bytes.
          // Species/iteration hooks must not become an alternate copy path.
          for (const name of [
            "buffer",
            "byteOffset",
            "byteLength",
            "length",
            "constructor",
            Symbol.iterator,
          ]) {
            Object.defineProperty(chunk, name, {
              get: () => {
                calls += 1;
                return 0;
              },
            });
          }
        }

        async function* source(): AsyncGenerator<Uint8Array> {
          await Promise.resolve();
          yield new Uint8Array();
          yield chunk;
          // The pump must drain the entire view before requesting another chunk.
          new Uint8Array(backing).fill(42);
        }

        const key = `${shared}-${streamed}-${overrides}`;

        await store.putObject({
          bucket: "bucket",
          key,
          body: streamed ? source() : chunk,
        });

        const selected = await store.getObject({ bucket: "bucket", key });
        await using body = selected.body;
        assert.equal(selected.contentLength, BigInt(expected.length), key);

        assert.deepEqual(
          new Uint8Array(await body.readAll({ maxBytes: expected.length })),
          expected,
          key,
        );

        assert.equal(calls, 0, key);
      }
    }
  }
});

await test("parity: expected owners are checked by every applicable operation", async (context) => {
  const { store, path } = await fixture(context);

  // A separate process owns the system SQLite library, never this native host.
  const accountId = execFileSync(
    process.execPath,
    [
      "--input-type=module",
      "--eval",
      `
        import { DatabaseSync } from "node:sqlite";

        const db = new DatabaseSync(process.argv[1] + "/metadata.sqlite3", {
          readOnly: true,
        });

        console.log(
          db
            .prepare(
              "SELECT owner_account_id FROM store_metadata WHERE singleton_id=1",
            )
            .get().owner_account_id,
        );

        db.close();
      `,
      path,
    ],
    { encoding: "utf8", timeout: 10_000 },
  ).trim();

  const wrongAccountId =
    accountId === "000000000000" ? "000000000001" : "000000000000";

  await store.headBucket({ bucket: "bucket", expectedBucketOwner: accountId });

  await store.getBucketLocation({
    bucket: "bucket",
    expectedBucketOwner: accountId,
  });

  await store.putObject({
    bucket: "bucket",
    key: "key",
    body: Buffer.from("value"),
    expectedBucketOwner: accountId,
  });

  await store.headObject({
    bucket: "bucket",
    key: "key",
    expectedBucketOwner: accountId,
  });

  await using body = (
    await store.getObject({
      bucket: "bucket",
      key: "key",
      expectedBucketOwner: accountId,
    })
  ).body;

  assert.equal(
    Buffer.from(await body.readAll({ maxBytes: 5 })).toString(),
    "value",
  );

  const input = {
    bucket: "bucket",
    key: "key",
    expectedBucketOwner: wrongAccountId,
  };

  await assert.rejects(
    store.headBucket({ bucket: "bucket", expectedBucketOwner: wrongAccountId }),
    failure("AccessDenied"),
  );

  await assert.rejects(
    store.getBucketLocation({
      bucket: "bucket",
      expectedBucketOwner: wrongAccountId,
    }),
    failure("AccessDenied"),
  );

  await assert.rejects(
    store.deleteBucket({
      bucket: "bucket",
      expectedBucketOwner: wrongAccountId,
    }),
    failure("AccessDenied", "NotCommitted"),
  );

  await assert.rejects(store.headObject(input), failure("AccessDenied"));
  await assert.rejects(store.getObject(input), failure("AccessDenied"));

  await assert.rejects(
    store.deleteObject(input),
    failure("AccessDenied", "NotCommitted"),
  );

  await assert.rejects(
    store.putObject({ ...input, body: Buffer.from("replacement") }),
    failure("AccessDenied", "NotCommitted"),
  );

  await store.deleteObject({ ...input, expectedBucketOwner: accountId });

  await store.deleteBucket({
    bucket: "bucket",
    expectedBucketOwner: accountId,
  });
});

for (const mode of ["read", "write"] as const) {
  await test(`parity: terminating a worker during persistent ${mode} preserves its peer`, async (context) => {
    const { store, path } = await fixture(context);

    await store.putObject({
      bucket: "bucket",
      key: "key",
      body: Buffer.alloc(131072, 97),
    });

    const worker = new Worker(
      new URL("./persistent-worker.mjs", import.meta.url),
      { workerData: { directory: path, mode } },
    );

    context.after(async () => {
      await worker.terminate();
    });

    const ready = await new Promise<unknown>((resolve, reject) => {
      worker.once("message", resolve);
      worker.once("error", reject);

      worker.once("exit", (status) =>
        reject(new Error(`worker exited before barrier: ${status}`)),
      );
    });

    assert.equal(ready, "selected");

    await store.putObject({
      bucket: "bucket",
      key: "key",
      body: Buffer.from("replacement"),
    });

    await worker.terminate();

    await using body = (await store.getObject({ bucket: "bucket", key: "key" }))
      .body;

    assert.equal(
      Buffer.from(await body.readAll({ maxBytes: 11 })).toString(),
      "replacement",
    );

    await store.close();
    await using reopened = await Store.open({ directory: path });

    assert.equal(
      (await reopened.headObject({ bucket: "bucket", key: "key" }))
        .contentLength,
      11n,
    );
  });
}
