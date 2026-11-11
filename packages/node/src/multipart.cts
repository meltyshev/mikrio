import type * as Values from "./multipart-values.cjs";
import type * as Native from "./multipart-native.cjs";
import type { NativeStore } from "./native.cjs";
import type { CallOptions } from "./index.cjs";
import type { ChecksumType } from "./values.cjs";
import {
  record,
  string,
  optionalString,
  optionalCount,
  instant,
  metadata,
  tags,
  algorithm,
  source,
} from "./inputs.cjs";
import { beforeMutation, StorageError } from "./errors.cjs";
import { invoke } from "./calls.cjs";
import { transfer } from "./upload.cjs";

function count(value: unknown): number {
  if (typeof value !== "number") {
    throw new TypeError("expected a number");
  }

  return value;
}

function contentLength(value: unknown): bigint | undefined {
  if (value === undefined) {
    return undefined;
  }

  if (typeof value !== "bigint") {
    throw new TypeError("contentLength must be a bigint");
  }

  return value;
}

function checksumType(value: unknown): ChecksumType | undefined {
  if (value === undefined) {
    return undefined;
  }

  if (value === "FULL_OBJECT" || value === "COMPOSITE") {
    return value;
  }

  throw new StorageError("InvalidArgument", "unsupported checksum type");
}

function directive(value: unknown): Values.CopyDirective | undefined {
  if (value === undefined) {
    return undefined;
  }

  if (value === "COPY" || value === "REPLACE") {
    return value;
  }

  throw new StorageError("InvalidArgument", "unsupported copy directive");
}

function objectRef(value: unknown): Values.ObjectRef {
  const input = record(value, ["bucket", "key"], "ObjectRef");
  return { bucket: string(input.bucket), key: string(input.key) };
}

function parts(value: unknown): readonly Values.CompletedPart[] {
  if (!Array.isArray(value)) {
    throw new TypeError("parts must be an array");
  }

  if (value.length > 10000) {
    throw new StorageError(
      "InvalidArgument",
      "at most 10000 parts are accepted",
    );
  }

  return Array.from(value, (value: unknown) => {
    const part = record(
      value,
      [
        "partNumber",
        "etag",
        "checksumCrc32",
        "checksumCrc32c",
        "checksumSha1",
        "checksumSha256",
        "checksumCrc64Nvme",
      ],
      "CompletedPart",
    );

    return {
      partNumber: count(part.partNumber),
      etag: string(part.etag),
      checksumCrc32: optionalString(part.checksumCrc32),
      checksumCrc32c: optionalString(part.checksumCrc32c),
      checksumSha1: optionalString(part.checksumSha1),
      checksumSha256: optionalString(part.checksumSha256),
      checksumCrc64Nvme: optionalString(part.checksumCrc64Nvme),
    };
  });
}

function copyObjectInput(value: unknown): Native.NativeCopyObjectInput {
  const input = record(
    value,
    [
      "bucket",
      "key",
      "copySource",
      "expectedBucketOwner",
      "expectedSourceBucketOwner",
      "copySourceIfMatch",
      "copySourceIfNoneMatch",
      "copySourceIfModifiedSince",
      "copySourceIfUnmodifiedSince",
      "metadataDirective",
      "contentType",
      "contentEncoding",
      "contentDisposition",
      "contentLanguage",
      "cacheControl",
      "expires",
      "metadata",
      "tags",
      "taggingDirective",
      "checksumAlgorithm",
      "ifMatch",
      "ifNoneMatch",
    ],
    "CopyObjectInput",
  );

  return {
    bucket: string(input.bucket),
    key: string(input.key),
    copySource: objectRef(input.copySource),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    expectedSourceBucketOwner: optionalString(input.expectedSourceBucketOwner),
    copySourceIfMatch: optionalString(input.copySourceIfMatch),
    copySourceIfNoneMatch: optionalString(input.copySourceIfNoneMatch),
    copySourceIfModifiedSince: instant(input.copySourceIfModifiedSince),
    copySourceIfUnmodifiedSince: instant(input.copySourceIfUnmodifiedSince),
    metadataDirective: directive(input.metadataDirective),
    contentType: optionalString(input.contentType),
    contentEncoding: optionalString(input.contentEncoding),
    contentDisposition: optionalString(input.contentDisposition),
    contentLanguage: optionalString(input.contentLanguage),
    cacheControl: optionalString(input.cacheControl),
    expires: instant(input.expires),
    metadata: metadata(input.metadata),
    tags: tags(input.tags),
    taggingDirective: directive(input.taggingDirective),
    checksumAlgorithm: algorithm(input.checksumAlgorithm),
    ifMatch: optionalString(input.ifMatch),
    ifNoneMatch: optionalString(input.ifNoneMatch),
  };
}

export async function copyObject(
  native: NativeStore,
  input: Values.CopyObjectInput,
  options: CallOptions | undefined,
): Promise<Values.CopyObjectOutput> {
  let fields: Native.NativeCopyObjectInput;

  try {
    fields = copyObjectInput(input);
  } catch (error: unknown) {
    throw beforeMutation(error);
  }

  const output = await invoke(options, true, (cancellation) =>
    native.copyObject(fields, cancellation),
  );

  return Object.freeze({
    ...output,
    lastModified: new Date(output.lastModified),
  });
}

function createMultipartUploadInput(
  value: unknown,
): Native.NativeCreateMultipartUploadInput {
  const input = record(
    value,
    [
      "bucket",
      "key",
      "expectedBucketOwner",
      "contentType",
      "contentEncoding",
      "contentDisposition",
      "contentLanguage",
      "cacheControl",
      "expires",
      "metadata",
      "tags",
      "checksumAlgorithm",
      "checksumType",
    ],
    "CreateMultipartUploadInput",
  );

  return {
    bucket: string(input.bucket),
    key: string(input.key),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    contentType: optionalString(input.contentType),
    contentEncoding: optionalString(input.contentEncoding),
    contentDisposition: optionalString(input.contentDisposition),
    contentLanguage: optionalString(input.contentLanguage),
    cacheControl: optionalString(input.cacheControl),
    expires: instant(input.expires),
    metadata: metadata(input.metadata),
    tags: tags(input.tags),
    checksumAlgorithm: algorithm(input.checksumAlgorithm),
    checksumType: checksumType(input.checksumType),
  };
}

export async function createMultipartUpload(
  native: NativeStore,
  input: Values.CreateMultipartUploadInput,
  options: CallOptions | undefined,
): Promise<Values.CreateMultipartUploadOutput> {
  let fields: Native.NativeCreateMultipartUploadInput;

  try {
    fields = createMultipartUploadInput(input);
  } catch (error: unknown) {
    throw beforeMutation(error);
  }

  const output = await invoke(options, true, (cancellation) =>
    native.createMultipartUpload(fields, cancellation),
  );

  return Object.freeze({ ...output });
}

function uploadPartInput(value: unknown): Native.NativeUploadPartInput {
  const input = record(
    value,
    [
      "bucket",
      "key",
      "uploadId",
      "partNumber",
      "body",
      "expectedBucketOwner",
      "contentLength",
      "checksumAlgorithm",
      "contentMd5",
      "checksumCrc32",
      "checksumCrc32c",
      "checksumSha1",
      "checksumSha256",
      "checksumCrc64Nvme",
    ],
    "UploadPartInput",
  );

  return {
    bucket: string(input.bucket),
    key: string(input.key),
    uploadId: string(input.uploadId),
    partNumber: count(input.partNumber),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    contentLength: contentLength(input.contentLength),
    checksumAlgorithm: algorithm(input.checksumAlgorithm),
    contentMd5: optionalString(input.contentMd5),
    checksumCrc32: optionalString(input.checksumCrc32),
    checksumCrc32c: optionalString(input.checksumCrc32c),
    checksumSha1: optionalString(input.checksumSha1),
    checksumSha256: optionalString(input.checksumSha256),
    checksumCrc64Nvme: optionalString(input.checksumCrc64Nvme),
  };
}

export async function uploadPart(
  native: NativeStore,
  input: Values.UploadPartInput,
  options: CallOptions | undefined,
): Promise<Values.UploadPartOutput> {
  let fields: Native.NativeUploadPartInput;
  let body: Uint8Array | AsyncIterable<Uint8Array>;

  try {
    fields = uploadPartInput(input);
    body = source(input.body);
  } catch (error: unknown) {
    throw beforeMutation(error);
  }

  return transfer(
    (upload, cancellation) => native.uploadPart(fields, upload, cancellation),
    body,
    options,
  );
}

function uploadPartCopyInput(value: unknown): Native.NativeUploadPartCopyInput {
  const input = record(
    value,
    [
      "bucket",
      "key",
      "uploadId",
      "partNumber",
      "copySource",
      "expectedBucketOwner",
      "expectedSourceBucketOwner",
      "copySourceIfMatch",
      "copySourceIfNoneMatch",
      "copySourceIfModifiedSince",
      "copySourceIfUnmodifiedSince",
      "copySourceRange",
    ],
    "UploadPartCopyInput",
  );

  return {
    bucket: string(input.bucket),
    key: string(input.key),
    uploadId: string(input.uploadId),
    partNumber: count(input.partNumber),
    copySource: objectRef(input.copySource),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    expectedSourceBucketOwner: optionalString(input.expectedSourceBucketOwner),
    copySourceIfMatch: optionalString(input.copySourceIfMatch),
    copySourceIfNoneMatch: optionalString(input.copySourceIfNoneMatch),
    copySourceIfModifiedSince: instant(input.copySourceIfModifiedSince),
    copySourceIfUnmodifiedSince: instant(input.copySourceIfUnmodifiedSince),
    copySourceRange: optionalString(input.copySourceRange),
  };
}

export async function uploadPartCopy(
  native: NativeStore,
  input: Values.UploadPartCopyInput,
  options: CallOptions | undefined,
): Promise<Values.UploadPartCopyOutput> {
  let fields: Native.NativeUploadPartCopyInput;

  try {
    fields = uploadPartCopyInput(input);
  } catch (error: unknown) {
    throw beforeMutation(error);
  }

  const output = await invoke(options, true, (cancellation) =>
    native.uploadPartCopy(fields, cancellation),
  );

  return Object.freeze({
    ...output,
    lastModified: new Date(output.lastModified),
  });
}

function completeMultipartUploadInput(
  value: unknown,
): Native.NativeCompleteMultipartUploadInput {
  const input = record(
    value,
    [
      "bucket",
      "key",
      "uploadId",
      "parts",
      "expectedBucketOwner",
      "ifMatch",
      "ifNoneMatch",
      "contentLength",
      "checksumType",
      "checksumCrc32",
      "checksumCrc32c",
      "checksumSha1",
      "checksumSha256",
      "checksumCrc64Nvme",
    ],
    "CompleteMultipartUploadInput",
  );

  return {
    bucket: string(input.bucket),
    key: string(input.key),
    uploadId: string(input.uploadId),
    parts: parts(input.parts),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    ifMatch: optionalString(input.ifMatch),
    ifNoneMatch: optionalString(input.ifNoneMatch),
    contentLength: contentLength(input.contentLength),
    checksumType: checksumType(input.checksumType),
    checksumCrc32: optionalString(input.checksumCrc32),
    checksumCrc32c: optionalString(input.checksumCrc32c),
    checksumSha1: optionalString(input.checksumSha1),
    checksumSha256: optionalString(input.checksumSha256),
    checksumCrc64Nvme: optionalString(input.checksumCrc64Nvme),
  };
}

export async function completeMultipartUpload(
  native: NativeStore,
  input: Values.CompleteMultipartUploadInput,
  options: CallOptions | undefined,
): Promise<Values.CompleteMultipartUploadOutput> {
  let fields: Native.NativeCompleteMultipartUploadInput;

  try {
    fields = completeMultipartUploadInput(input);
  } catch (error: unknown) {
    throw beforeMutation(error);
  }

  const output = await invoke(options, true, (cancellation) =>
    native.completeMultipartUpload(fields, cancellation),
  );

  return Object.freeze({ ...output });
}

function abortMultipartUploadInput(
  value: unknown,
): Native.NativeAbortMultipartUploadInput {
  const input = record(
    value,
    ["bucket", "key", "uploadId", "expectedBucketOwner"],
    "AbortMultipartUploadInput",
  );

  return {
    bucket: string(input.bucket),
    key: string(input.key),
    uploadId: string(input.uploadId),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
  };
}

export async function abortMultipartUpload(
  native: NativeStore,
  input: Values.AbortMultipartUploadInput,
  options: CallOptions | undefined,
): Promise<Values.AbortMultipartUploadOutput> {
  let fields: Native.NativeAbortMultipartUploadInput;

  try {
    fields = abortMultipartUploadInput(input);
  } catch (error: unknown) {
    throw beforeMutation(error);
  }

  await invoke(options, true, (cancellation) =>
    native.abortMultipartUpload(fields, cancellation),
  );

  return Object.freeze({});
}

function listPartsInput(value: unknown): Native.NativeListPartsInput {
  const input = record(
    value,
    [
      "bucket",
      "key",
      "uploadId",
      "expectedBucketOwner",
      "partNumberMarker",
      "maxParts",
    ],
    "ListPartsInput",
  );

  return {
    bucket: string(input.bucket),
    key: string(input.key),
    uploadId: string(input.uploadId),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    partNumberMarker: optionalCount(input.partNumberMarker),
    maxParts: optionalCount(input.maxParts),
  };
}

export async function listParts(
  native: NativeStore,
  input: Values.ListPartsInput,
  options: CallOptions | undefined,
): Promise<Values.ListPartsOutput> {
  const fields = listPartsInput(input);

  const output = await invoke(options, false, (cancellation) =>
    native.listParts(fields, cancellation),
  );

  return Object.freeze({
    ...output,
    parts: Object.freeze(
      output.parts.map((part) =>
        Object.freeze({ ...part, lastModified: new Date(part.lastModified) }),
      ),
    ),
  });
}

function listMultipartUploadsInput(
  value: unknown,
): Native.NativeListMultipartUploadsInput {
  const input = record(
    value,
    [
      "bucket",
      "expectedBucketOwner",
      "prefix",
      "delimiter",
      "keyMarker",
      "uploadIdMarker",
      "maxUploads",
    ],
    "ListMultipartUploadsInput",
  );

  return {
    bucket: string(input.bucket),
    expectedBucketOwner: optionalString(input.expectedBucketOwner),
    prefix: optionalString(input.prefix),
    delimiter: optionalString(input.delimiter),
    keyMarker: optionalString(input.keyMarker),
    uploadIdMarker: optionalString(input.uploadIdMarker),
    maxUploads: optionalCount(input.maxUploads),
  };
}

export async function listMultipartUploads(
  native: NativeStore,
  input: Values.ListMultipartUploadsInput,
  options: CallOptions | undefined,
): Promise<Values.ListMultipartUploadsOutput> {
  const fields = listMultipartUploadsInput(input);

  const output = await invoke(options, false, (cancellation) =>
    native.listMultipartUploads(fields, cancellation),
  );

  return Object.freeze({
    ...output,
    uploads: Object.freeze(
      output.uploads.map((upload) =>
        Object.freeze({ ...upload, initiated: new Date(upload.initiated) }),
      ),
    ),
    commonPrefixes: Object.freeze(
      output.commonPrefixes.map((prefix) => Object.freeze({ prefix })),
    ),
  });
}
