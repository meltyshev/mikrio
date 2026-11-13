import { Store, type OpenOptions } from "@mikrio/node";

export async function commonjs(directory: string): Promise<string> {
  const options: OpenOptions = { directory, region: undefined };
  await using store = await Store.open(options);

  await store.createBucket({ bucket: "bucket" });

  await store.putObject({
    bucket: "bucket",
    key: "key",
    body: Uint8Array.of(42),
  });

  const output = await store.getObject({ bucket: "bucket", key: "key" });

  await using body = output.body;
  const bytes: Uint8Array = await body.readAll({ maxBytes: 1 });
  await body.close();

  if (bytes[0] !== 42) {
    throw new Error("unexpected payload");
  }

  return store.info.storeId;
}
