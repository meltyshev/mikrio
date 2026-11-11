import { record, string, optionalString, optionalCount } from "./inputs.cjs";
import { StorageError } from "./errors.cjs";
import { invoke } from "./calls.cjs";
import type { NativeStore } from "./native.cjs";
import type { CallOptions } from "./index.cjs";
import type { ObjectAttribute, GetObjectAttributesOutput } from "./values.cjs";

function selector(value: unknown): ObjectAttribute {
  const name = string(value);

  switch (name) {
    case "ETag":
    case "Checksum":
    case "ObjectParts":
    case "StorageClass":
    case "ObjectSize":
      return name;
    default:
      throw new StorageError("InvalidArgument", "unknown attribute selector");
  }
}

function selectors(value: unknown): ObjectAttribute[] {
  if (!Array.isArray(value)) {
    throw new TypeError("objectAttributes must be an array");
  }

  const length = value.length;

  if (!Number.isInteger(length) || length < 1 || length > 5) {
    throw new StorageError(
      "InvalidArgument",
      "one to five selectors are required",
    );
  }

  const selected: ObjectAttribute[] = [];

  // Copy a bounded snapshot of array entries without invoking a custom iterator.
  for (let index = 0; index < length; index++) {
    selected.push(selector(value[index]));
  }

  return selected;
}

export async function get(
  store: NativeStore,
  value: unknown,
  options?: CallOptions,
): Promise<GetObjectAttributesOutput> {
  const fields = record(
    value,
    [
      "bucket",
      "key",
      "expectedBucketOwner",
      "objectAttributes",
      "maxParts",
      "partNumberMarker",
    ],
    "GetObjectAttributesInput",
  );

  const input = {
    bucket: string(fields.bucket),
    key: string(fields.key),
    expectedBucketOwner: optionalString(fields.expectedBucketOwner),
    objectAttributes: selectors(fields.objectAttributes),
    maxParts: optionalCount(fields.maxParts),
    partNumberMarker: optionalCount(fields.partNumberMarker),
  };

  const output = await invoke(options, false, (cancellation) =>
    store.getObjectAttributes(input, cancellation),
  );

  return Object.freeze({
    objectSize: output.objectSize,
    etag: output.etag,
    checksum:
      output.checksum === undefined
        ? undefined
        : Object.freeze({ ...output.checksum }),
    storageClass: output.storageClass,
    objectParts:
      output.objectParts === undefined
        ? undefined
        : Object.freeze({
            ...output.objectParts,
            parts: Object.freeze(
              output.objectParts.parts.map((part) =>
                Object.freeze({ ...part }),
              ),
            ),
          }),
    lastModified: new Date(output.lastModified),
  });
}
