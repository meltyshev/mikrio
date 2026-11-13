import assert from "node:assert/strict";
import { parentPort, workerData } from "node:worker_threads";
import { Store } from "@mikrio/node";

// The parent owns this fixture input and terminates the environment at a barrier.
const input: unknown = workerData;
assert.ok(typeof input === "object" && input !== null);
assert.ok("directory" in input && typeof input.directory === "string");

assert.ok(
  "mode" in input &&
    (input.mode === "read" ||
      input.mode === "write" ||
      input.mode === "listener"),
);

assert.ok(parentPort);
const port = parentPort;
await using store = await Store.open({ directory: input.directory });

const retained = new Promise<void>((resolve) => {
  port.once("message", resolve);
});

if (input.mode === "listener") {
  await using server = await store.listen();
  port.postMessage(server.endpoint);
  await retained;
  await server.close();
} else if (input.mode === "read") {
  await using body = (await store.getObject({ bucket: "bucket", key: "key" }))
    .body;

  assert.equal((await body.readChunk({ maxBytes: 1 }))?.[0], 97);
  port.postMessage("selected");
  await retained;
} else {
  async function* source(): AsyncGenerator<Uint8Array> {
    yield Buffer.from("unpublished prefix");
    port.postMessage("selected");
    await retained;
    yield Buffer.from("must never publish after teardown");
  }

  await store.putObject({ bucket: "bucket", key: "key", body: source() });
}

await store.close();
