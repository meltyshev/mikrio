import {
  Store,
  StorageError,
  type OpenOptions,
  type StoreInfo,
} from "@mikrio/node";

export async function positive(directory: string): Promise<StoreInfo> {
  const options: OpenOptions = { directory, region: undefined };
  await using store = await Store.open(options, { signal: undefined });

  await store.createBucket({ bucket: "bucket" });

  await store.putObject({
    bucket: "bucket",
    key: "key",
    body: new Uint8Array(),
    contentLength: 0n,
    metadata: undefined,
    tags: [{ key: "tag", value: "" }],
  });

  const output = await store.getObject(
    {
      bucket: "bucket",
      key: "key",
      checksumMode: "ENABLED",
      ifModifiedSince: new Date(0),
    },
    { signal: undefined },
  );

  await using body = output.body;

  const length: bigint = output.contentLength;
  const instant: Date = output.lastModified;
  await body.readAll({ maxBytes: 0 });
  await body.close();

  if (length !== 0n || instant.getTime() < 0) {
    throw new Error("unexpected facts");
  }

  const deleted = await store.deleteObjects(
    {
      bucket: "bucket",
      objects: [{ key: "key", etag: "*" }],
      quiet: undefined,
    },
    { signal: undefined },
  );

  const keys: readonly string[] = deleted.deleted.map((item) => item.key);

  if (keys.length !== 1) {
    throw new Error("unexpected batch result");
  }

  await store.deleteObject({ bucket: "bucket", key: "key" });
  await store.deleteBucket({ bucket: "bucket" });
  return store.info;
}

// Compiled only, never invoked: these exercise the installed public declarations.
export function negative(store: Store, error: StorageError): void {
  // @ts-expect-error null is not an omitted region.
  Store.open({ directory: "x", region: null });
  // @ts-expect-error unknown input fields are rejected.
  Store.open({ directory: "x", reset: true });
  // @ts-expect-error close has no cancellation argument.
  store.close({ signal: new AbortController().signal });
  // @ts-expect-error store facts are readonly.
  store.info.region = "changed";
  // @ts-expect-error construction requires native acquisition.
  new Store();

  store.putObject({
    bucket: "bucket",
    key: "key",
    body: new Uint8Array(),
    // @ts-expect-error object byte counts use bigint, never number.
    contentLength: 1,
  });

  store.putObject({
    bucket: "bucket",
    key: "key",
    body: new Uint8Array(),
    // @ts-expect-error omitted content type is undefined, never null.
    contentType: null,
  });

  // @ts-expect-error ranges use HTTP byte-range strings, never numeric offsets.
  store.getObject({ bucket: "bucket", key: "key", range: 0 });

  store.headObject({
    bucket: "bucket",
    key: "key",
    // @ts-expect-error response expiry uses the shared Date instant type.
    responseExpires: "tomorrow",
  });

  store.deleteObjects({
    bucket: "bucket",
    objects: [{ key: "key" }],
    // @ts-expect-error quiet is a boolean or undefined.
    quiet: null,
  });

  store.deleteObjects({
    bucket: "bucket",
    objects: [{ key: "key" }],
    // @ts-expect-error per-entry ETag conditions are not batch-wide conditions.
    ifMatch: "*",
  });

  if (error.partialResult) {
    const item = error.partialResult.items[0];

    if (item) {
      const outcome: "Committed" | "NotCommitted" | "Unknown" =
        item.mutationOutcome;

      // @ts-expect-error detached progress is immutable.
      item.index = 2;
      console.log(outcome);
    }
  }

  const code: string = error.code;
  console.log(code);
}
