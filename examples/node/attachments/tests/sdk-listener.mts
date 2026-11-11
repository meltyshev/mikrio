// An ordinary installed package supplies the same public fixture as listener_sdk.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile, realpath, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { createInterface } from "node:readline";
import { Store } from "mikrio";

const directory = process.env["mikrio_fixture_store"];
const session = process.env["mikrio_fixture_session"];
assert.ok(directory && session);
const require = createRequire(import.meta.url);
const installed = await realpath(require.resolve("mikrio"));
assert.ok(installed.startsWith((await realpath(process.cwd())) + "/"));

if (process.argv[2] === "inspect") {
  const key = process.argv[3];
  assert.ok(key);
  await using store = await Store.open({ directory });

  const object = await store.getObject({
    bucket: "photos",
    key,
    checksumMode: "ENABLED",
  });

  try {
    const digest = createHash("sha256");
    let length = 0;

    for (;;) {
      const chunk = await object.body.readChunk({ maxBytes: 64 * 1024 });

      if (chunk === null) {
        break;
      }

      digest.update(chunk);
      length += chunk.length;
    }

    process.stdout.write(
      JSON.stringify({
        length,
        sha256: digest.digest("hex"),
        checksumSha256: object.checksumSha256,
      }) + "\n",
    );
  } finally {
    await object.body.close();
  }
} else if (process.argv[2] === "revoke") {
  const fields = (await readFile(session, "utf8")).trimEnd().split("\n");
  assert.equal(fields.length, 4);
  const [, principalId, accessKeyId] = fields;
  assert.ok(principalId && accessKeyId);
  await using store = await Store.open({ directory });
  await store.deleteAccessKey({ principalId, accessKeyId });
} else {
  {
    await using resources = new AsyncDisposableStack();
    const store = await Store.open({ directory });
    resources.defer(() => store.close());
    const peer = await Store.open({ directory });
    resources.defer(() => peer.close());
    await store.createBucket({ bucket: "photos" });

    await store.putObject({
      bucket: "photos",
      key: "native-seed",
      body: Buffer.from("native-to-sdk"),
      contentType: "text/plain",
      metadata: { title: "café" },
    });

    const principal = await store.createPrincipal({ name: "sdk-app" });
    const policy: unknown = JSON.parse(
      await readFile(
        new URL("./sdk-listener-policy.json", import.meta.url),
        "utf8",
      ),
    );

    await store.putPrincipalPolicy({
      principalId: principal.principalId,
      document: JSON.stringify(policy),
    });

    const key = await store.createAccessKey({
      principalId: principal.principalId,
    });
    process.stdout.write("mikrio_prepared\n");
    const first = await store.listen({ host: "127.0.0.1", port: 0 });
    resources.defer(() => first.close());
    const second = await peer.listen({ host: "127.0.0.1", port: 0 });
    resources.defer(() => second.close());

    await writeFile(
      session,
      [
        second.endpoint,
        principal.principalId,
        key.accessKey.accessKeyId,
        key.secretAccessKey,
        "",
      ].join("\n"),
      { mode: 0o600 },
    );

    process.stdout.write(`mikrio_ready ${first.endpoint}\n`);
    const input = createInterface({ input: process.stdin });

    try {
      const line = await input[Symbol.asyncIterator]().next();
      assert.equal(line.value, "stop");
    } finally {
      input.close();
    }

    const keys = await peer.listAccessKeys({
      principalId: principal.principalId,
    });

    assert.equal(keys.accessKeys.length, 0);
    await first.close();
    const replacement = await store.listen({ host: "127.0.0.1", port: 0 });
    await replacement.close();
    await peer.close();
    await second.close();
    await verify(store);
    await store.close();
    await using reopened = await Store.open({ directory });
    await verify(reopened);
  }

  process.stdout.write(
    'mikrio_observed {"reopened_objects":2,"verified_bytes":532480,"listeners":2,"revoked_keys":1}\n',
  );
}

async function verify(store: Store): Promise<void> {
  for (const [key, length, modulus] of [
    ["boto-shared", 257 * 1024, 251],
    ["node-shared", 263 * 1024, 239],
  ] as const) {
    const object = await store.getObject({
      bucket: "photos",
      key,
      checksumMode: "ENABLED",
    });

    try {
      assert.equal(object.contentLength, BigInt(length));
      assert.ok(object.checksumCrc32);
      let offset = 0;

      for (;;) {
        const chunk = await object.body.readChunk({ maxBytes: 64 * 1024 });

        if (chunk === null) {
          break;
        }

        for (const byte of chunk) {
          assert.equal(byte, offset % modulus);
          offset += 1;
        }
      }

      assert.equal(offset, length);
    } finally {
      await object.body.close();
    }
  }
}
