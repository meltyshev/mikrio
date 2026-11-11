import type {
  Tag,
  ChecksumAlgorithm,
  ChecksumType,
  CommonPrefix,
} from "./values.cjs";

/** Select source metadata/tags or the caller's complete replacement. */
export type CopyDirective = "COPY" | "REPLACE";

/** A literal source bucket and key; no URL decoding or path normalization. */
export interface ObjectRef {
  readonly bucket: string;
  readonly key: string;
}

/** A selected uploaded part, identified by its number and returned ETag. */
export interface CompletedPart {
  readonly partNumber: number;
  readonly etag: string;
  readonly checksumCrc32?: string | undefined;
  readonly checksumCrc32c?: string | undefined;
  readonly checksumSha1?: string | undefined;
  readonly checksumSha256?: string | undefined;
  readonly checksumCrc64Nvme?: string | undefined;
}

/** Copy a source object; source conditions and destination conditions are separate. */
export interface CopyObjectInput {
  bucket: string;
  key: string;
  copySource: ObjectRef;
  expectedBucketOwner?: string | undefined;
  expectedSourceBucketOwner?: string | undefined;
  copySourceIfMatch?: string | undefined;
  copySourceIfNoneMatch?: string | undefined;
  copySourceIfModifiedSince?: Date | undefined;
  copySourceIfUnmodifiedSince?: Date | undefined;
  metadataDirective?: CopyDirective | undefined;
  contentType?: string | undefined;
  contentEncoding?: string | undefined;
  contentDisposition?: string | undefined;
  contentLanguage?: string | undefined;
  cacheControl?: string | undefined;
  expires?: Date | undefined;
  metadata?: Readonly<Record<string, string>> | undefined;
  tags?: readonly Tag[] | undefined;
  taggingDirective?: CopyDirective | undefined;
  checksumAlgorithm?: ChecksumAlgorithm | undefined;
  ifMatch?: string | undefined;
  ifNoneMatch?: string | undefined;
}

/** Capture object metadata and checksum selection for a durable multipart upload. */
export interface CreateMultipartUploadInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
  contentType?: string | undefined;
  contentEncoding?: string | undefined;
  contentDisposition?: string | undefined;
  contentLanguage?: string | undefined;
  cacheControl?: string | undefined;
  expires?: Date | undefined;
  metadata?: Readonly<Record<string, string>> | undefined;
  tags?: readonly Tag[] | undefined;
  checksumAlgorithm?: ChecksumAlgorithm | undefined;
  checksumType?: ChecksumType | undefined;
}

/** Stream one part; retain mutable bytes until the next pull or call settlement. */
export interface UploadPartInput {
  bucket: string;
  key: string;
  uploadId: string;
  partNumber: number;
  body: Uint8Array | AsyncIterable<Uint8Array>;
  expectedBucketOwner?: string | undefined;
  contentLength?: bigint | undefined;
  checksumAlgorithm?: ChecksumAlgorithm | undefined;
  contentMd5?: string | undefined;
  checksumCrc32?: string | undefined;
  checksumCrc32c?: string | undefined;
  checksumSha1?: string | undefined;
  checksumSha256?: string | undefined;
  checksumCrc64Nvme?: string | undefined;
}

/** Select a source object or inclusive byte range for one upload part. */
export interface UploadPartCopyInput {
  bucket: string;
  key: string;
  uploadId: string;
  partNumber: number;
  copySource: ObjectRef;
  expectedBucketOwner?: string | undefined;
  expectedSourceBucketOwner?: string | undefined;
  copySourceIfMatch?: string | undefined;
  copySourceIfNoneMatch?: string | undefined;
  copySourceIfModifiedSince?: Date | undefined;
  copySourceIfUnmodifiedSince?: Date | undefined;
  copySourceRange?: string | undefined;
}

/** Publish one ordered selection of parts; conditions apply to the destination. */
export interface CompleteMultipartUploadInput {
  bucket: string;
  key: string;
  uploadId: string;
  parts: readonly CompletedPart[];
  expectedBucketOwner?: string | undefined;
  ifMatch?: string | undefined;
  ifNoneMatch?: string | undefined;
  contentLength?: bigint | undefined;
  checksumType?: ChecksumType | undefined;
  checksumCrc32?: string | undefined;
  checksumCrc32c?: string | undefined;
  checksumSha1?: string | undefined;
  checksumSha256?: string | undefined;
  checksumCrc64Nvme?: string | undefined;
}

/** Identify the durable upload to retire, without deleting a completed object. */
export interface AbortMultipartUploadInput {
  bucket: string;
  key: string;
  uploadId: string;
  expectedBucketOwner?: string | undefined;
}

/** Read a bounded part page, continuing strictly after partNumberMarker. */
export interface ListPartsInput {
  bucket: string;
  key: string;
  uploadId: string;
  expectedBucketOwner?: string | undefined;
  partNumberMarker?: number | undefined;
  maxParts?: number | undefined;
}

/** Read a live upload page with literal prefix/delimiter and paired markers. */
export interface ListMultipartUploadsInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
  prefix?: string | undefined;
  delimiter?: string | undefined;
  keyMarker?: string | undefined;
  uploadIdMarker?: string | undefined;
  maxUploads?: number | undefined;
}

export interface CopyObjectOutput {
  readonly etag: string;
  readonly lastModified: Date;
  readonly checksumCrc32?: string | undefined;
  readonly checksumCrc32c?: string | undefined;
  readonly checksumSha1?: string | undefined;
  readonly checksumSha256?: string | undefined;
  readonly checksumCrc64Nvme?: string | undefined;
}

export interface CreateMultipartUploadOutput {
  readonly uploadId: string;
  readonly checksumAlgorithm: ChecksumAlgorithm;
  readonly checksumType: ChecksumType;
}

export interface UploadPartOutput {
  readonly etag: string;
  readonly checksumCrc32?: string | undefined;
  readonly checksumCrc32c?: string | undefined;
  readonly checksumSha1?: string | undefined;
  readonly checksumSha256?: string | undefined;
  readonly checksumCrc64Nvme?: string | undefined;
}

export interface UploadPartCopyOutput {
  readonly etag: string;
  readonly lastModified: Date;
  readonly checksumCrc32?: string | undefined;
  readonly checksumCrc32c?: string | undefined;
  readonly checksumSha1?: string | undefined;
  readonly checksumSha256?: string | undefined;
  readonly checksumCrc64Nvme?: string | undefined;
}

export interface CompleteMultipartUploadOutput {
  readonly etag: string;
  readonly checksumType: ChecksumType;
  readonly checksumCrc32?: string | undefined;
  readonly checksumCrc32c?: string | undefined;
  readonly checksumSha1?: string | undefined;
  readonly checksumSha256?: string | undefined;
  readonly checksumCrc64Nvme?: string | undefined;
}

export type AbortMultipartUploadOutput = Readonly<Record<string, never>>;

/** Detached facts about one uploaded part; size is measured in bytes. */
export interface PartSummary {
  readonly partNumber: number;
  readonly size: bigint;
  readonly etag: string;
  readonly lastModified: Date;
  readonly checksumCrc32?: string | undefined;
  readonly checksumCrc32c?: string | undefined;
  readonly checksumSha1?: string | undefined;
  readonly checksumSha256?: string | undefined;
  readonly checksumCrc64Nvme?: string | undefined;
}

export interface MultipartUploadSummary {
  readonly key: string;
  readonly uploadId: string;
  readonly initiated: Date;
  readonly checksumAlgorithm: ChecksumAlgorithm;
  readonly checksumType: ChecksumType;
}

export interface ListPartsOutput {
  readonly parts: readonly PartSummary[];
  readonly isTruncated: boolean;
  readonly nextPartNumberMarker?: number | undefined;
  readonly checksumAlgorithm: ChecksumAlgorithm;
  readonly checksumType: ChecksumType;
}

export interface ListMultipartUploadsOutput {
  readonly uploads: readonly MultipartUploadSummary[];
  readonly commonPrefixes: readonly CommonPrefix[];
  readonly isTruncated: boolean;
  readonly nextKeyMarker?: string | undefined;
  readonly nextUploadIdMarker?: string | undefined;
}
