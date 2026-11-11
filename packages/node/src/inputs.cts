import { isUint8Array } from "node:util/types";
import { StorageError } from "./errors.cjs";
import type {
  NativePutInput,
  NativeReadInput,
  NativeReadOutput,
} from "./native.cjs";
import type {
  ChecksumAlgorithm,
  HeadObjectOutput,
  Tag,
  ListObjectsV2Input,
  ListBucketsInput,
  DeleteObjectsInput,
} from "./values.cjs";

export function record(
  value: unknown,
  fields: readonly string[],
  label: string,
): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object`);
  }

  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== "string" || !fields.includes(key)) {
      throw new TypeError(`${label} has an unknown field`);
    }
  }

  // Own-key validation establishes an object bag; each value remains unknown.
  return value as Record<string, unknown>;
}

export function string(value: unknown): string {
  if (typeof value !== "string") {
    throw new TypeError("expected a string");
  }

  return value;
}

export function optionalString(value: unknown): string | undefined {
  return value === undefined ? undefined : string(value);
}

export function deleteObjectsInput(value: unknown): DeleteObjectsInput {
  const input = record(
    value,
    ["bucket", "expectedBucketOwner", "objects", "quiet"],
    "DeleteObjectsInput",
  );

  if (!Array.isArray(input.objects)) {
    throw new TypeError("objects must be an array");
  }

  if (input.objects.length > 1000) {
    throw new StorageError(
      "InvalidArgument",
      "delete batch exceeds 1000 entries",
    );
  }

  if (input.quiet !== undefined && typeof input.quiet !== "boolean") {
    throw new TypeError("quiet must be a boolean");
  }

  return {
    bucket: string(input.bucket),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    quiet: input.quiet,
    objects: Array.from(input.objects, (value: unknown) => {
      const item = record(value, ["key", "etag"], "ObjectIdentifier");
      return { key: string(item.key), etag: optionalString(item.etag) };
    }),
  };
}

export function listBucketsInput(value: unknown): ListBucketsInput {
  const input = record(
    value,
    ["prefix", "maxBuckets", "continuationToken"],
    "ListBucketsInput",
  );

  if (input.maxBuckets !== undefined && typeof input.maxBuckets !== "number") {
    throw new TypeError("maxBuckets must be a number");
  }

  return {
    prefix: optionalString(input.prefix),
    maxBuckets: input.maxBuckets,
    continuationToken: optionalString(input.continuationToken),
  };
}

export function listObjectsInput(value: unknown): ListObjectsV2Input {
  const input = record(
    value,
    [
      "bucket",
      "expectedBucketOwner",
      "prefix",
      "delimiter",
      "maxKeys",
      "continuationToken",
      "startAfter",
    ],
    "ListObjectsV2Input",
  );

  if (input.maxKeys !== undefined && typeof input.maxKeys !== "number") {
    throw new TypeError("maxKeys must be a number");
  }

  return {
    bucket: string(input.bucket),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    prefix: optionalString(input.prefix),
    delimiter: optionalString(input.delimiter),
    maxKeys: input.maxKeys,
    continuationToken: optionalString(input.continuationToken),
    startAfter: optionalString(input.startAfter),
  };
}

export function instant(value: unknown): number | undefined {
  if (value === undefined) {
    return undefined;
  }

  if (!(value instanceof Date)) {
    throw new TypeError("expected a Date or undefined");
  }

  const milliseconds = Date.prototype.getTime.call(value);

  if (!Number.isFinite(milliseconds)) {
    throw new StorageError(
      "InvalidArgument",
      "Date must represent a valid instant",
    );
  }

  return milliseconds;
}

export function bucketInput(value: unknown): {
  bucket: string;
  expectedBucketOwner: string | undefined;
} {
  const input = record(
    value,
    ["bucket", "expectedBucketOwner"],
    "bucket input",
  );

  return {
    bucket: string(input.bucket),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
  };
}

export function readInput(value: unknown): NativeReadInput {
  const input = record(
    value,
    [
      "bucket",
      "key",
      "expectedBucketOwner",
      "checksumMode",
      "range",
      "partNumber",
      "responseContentType",
      "responseContentEncoding",
      "responseContentDisposition",
      "responseContentLanguage",
      "responseCacheControl",
      "responseExpires",
      "ifMatch",
      "ifNoneMatch",
      "ifModifiedSince",
      "ifUnmodifiedSince",
    ],
    "object read input",
  );

  const checksumMode = optionalString(input.checksumMode);

  if (checksumMode !== undefined && checksumMode !== "ENABLED") {
    throw new StorageError("InvalidArgument", "unsupported checksum mode");
  }

  return {
    bucket: string(input.bucket),
    key: string(input.key),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    checksumMode,
    range: optionalString(input.range),
    partNumber: optionalCount(input.partNumber),
    responseContentType: optionalString(input.responseContentType),
    responseContentEncoding: optionalString(input.responseContentEncoding),
    responseContentDisposition: optionalString(
      input.responseContentDisposition,
    ),
    responseContentLanguage: optionalString(input.responseContentLanguage),
    responseCacheControl: optionalString(input.responseCacheControl),
    responseExpires: instant(input.responseExpires),
    ifMatch: optionalString(input.ifMatch),
    ifNoneMatch: optionalString(input.ifNoneMatch),
    ifModifiedSince: instant(input.ifModifiedSince),
    ifUnmodifiedSince: instant(input.ifUnmodifiedSince),
  };
}

export function deleteInput(value: unknown): {
  bucket: string;
  key: string;
  expectedBucketOwner: string | undefined;
  ifMatch: string | undefined;
} {
  const input = record(
    value,
    ["bucket", "key", "expectedBucketOwner", "ifMatch"],
    "DeleteObjectInput",
  );

  return {
    bucket: string(input.bucket),
    key: string(input.key),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    ifMatch: optionalString(input.ifMatch),
  };
}

export function algorithm(value: unknown): ChecksumAlgorithm | undefined {
  const name = optionalString(value);

  switch (name) {
    case undefined:
    case "CRC32":
    case "CRC32C":
    case "CRC64NVME":
    case "SHA1":
    case "SHA256":
      return name;
    default:
      throw new StorageError(
        "InvalidArgument",
        "unsupported checksum algorithm",
      );
  }
}

export function tags(value: unknown): readonly Tag[] | undefined {
  if (value === undefined) {
    return undefined;
  }

  if (!Array.isArray(value)) {
    throw new TypeError("tags must be an array or undefined");
  }

  if (value.length > 10) {
    throw new StorageError("InvalidArgument", "at most ten tags are accepted");
  }

  return value.map((entry: unknown) => {
    const tag = record(entry, ["key", "value"], "Tag");
    return { key: string(tag.key), value: string(tag.value) };
  });
}

export function metadata(value: unknown): readonly Tag[] | undefined {
  if (value === undefined) {
    return undefined;
  }

  const isObjectMap =
    typeof value === "object" &&
    value !== null &&
    Object.prototype.toString.call(value) === "[object Object]";

  if (!isObjectMap) {
    throw new TypeError("metadata must be a string map or undefined");
  }

  const entries: Tag[] = [];

  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== "string") {
      throw new TypeError("metadata keys must be strings");
    }

    const field: unknown = Reflect.get(value, key);
    entries.push({ key, value: string(field) });
  }

  return entries;
}

export function putInput(value: unknown): {
  input: NativePutInput;
  body: Uint8Array | AsyncIterable<Uint8Array>;
} {
  const input = record(
    value,
    [
      "bucket",
      "key",
      "body",
      "expectedBucketOwner",
      "contentLength",
      "contentType",
      "contentEncoding",
      "contentDisposition",
      "contentLanguage",
      "cacheControl",
      "expires",
      "metadata",
      "tags",
      "checksumAlgorithm",
      "contentMd5",
      "checksumCrc32",
      "checksumCrc32c",
      "checksumSha1",
      "checksumSha256",
      "checksumCrc64Nvme",
      "ifMatch",
      "ifNoneMatch",
    ],
    "PutObjectInput",
  );

  if (input.contentLength !== undefined) {
    if (typeof input.contentLength !== "bigint") {
      throw new TypeError("contentLength must be a bigint or undefined");
    }
  }

  const body = source(input.body);

  return {
    body,
    input: {
      bucket: string(input.bucket),
      key: string(input.key),
      expectedBucketOwner: optionalString(input.expectedBucketOwner),
      contentLength: input.contentLength,
      contentType: optionalString(input.contentType),
      contentEncoding: optionalString(input.contentEncoding),
      contentDisposition: optionalString(input.contentDisposition),
      contentLanguage: optionalString(input.contentLanguage),
      cacheControl: optionalString(input.cacheControl),
      expires: instant(input.expires),
      metadata: metadata(input.metadata),
      tags: tags(input.tags),
      checksumAlgorithm: algorithm(input.checksumAlgorithm),
      contentMd5: optionalString(input.contentMd5),
      checksumCrc32: optionalString(input.checksumCrc32),
      checksumCrc32c: optionalString(input.checksumCrc32c),
      checksumSha1: optionalString(input.checksumSha1),
      checksumSha256: optionalString(input.checksumSha256),
      checksumCrc64Nvme: optionalString(input.checksumCrc64Nvme),
      ifMatch: optionalString(input.ifMatch),
      ifNoneMatch: optionalString(input.ifNoneMatch),
    },
  };
}

export function source(value: unknown): Uint8Array | AsyncIterable<Uint8Array> {
  if (isUint8Array(value)) {
    return value;
  }

  const isAsyncIterable =
    typeof value === "object" &&
    value !== null &&
    Symbol.asyncIterator in value &&
    typeof value[Symbol.asyncIterator] === "function";

  if (!isAsyncIterable) {
    throw new TypeError("body must be Uint8Array or AsyncIterable<Uint8Array>");
  }

  // Each next result and chunk is still checked by the host pump before copying.
  return value as AsyncIterable<Uint8Array>;
}

export function byteBound(value: unknown, positive: boolean): number {
  const input = record(value, ["maxBytes"], "body read input");

  if (typeof input.maxBytes !== "number") {
    throw new TypeError("maxBytes must be a number");
  }

  const hasInvalidBound =
    !Number.isSafeInteger(input.maxBytes) ||
    input.maxBytes < (positive ? 1 : 0);

  if (hasInvalidBound) {
    throw new StorageError(
      "InvalidArgument",
      "maxBytes must be a valid nonnegative byte bound",
    );
  }

  return input.maxBytes;
}

export function readOutput(output: NativeReadOutput): HeadObjectOutput {
  const entries: [string, string][] = output.metadata.map(({ key, value }) => [
    key,
    value,
  ]);

  const metadata = Object.fromEntries(entries);

  return Object.freeze({
    ...output,
    metadata: Object.freeze(metadata),
    lastModified: new Date(output.lastModified),
    expires:
      output.expires === undefined ? undefined : new Date(output.expires),
  });
}

export function optionalCount(value: unknown): number | undefined {
  if (value === undefined) {
    return undefined;
  }

  if (typeof value !== "number") {
    throw new TypeError("expected a number");
  }

  return value;
}
