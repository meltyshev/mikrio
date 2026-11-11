import type { Tag, ChecksumAlgorithm, ChecksumType } from "./values.cjs";
import type {
  ObjectRef,
  CompletedPart,
  CopyDirective,
} from "./multipart-values.cjs";

export interface NativeCopyObjectInput {
  bucket: string;
  key: string;
  copySource: ObjectRef;
  expectedBucketOwner?: string | undefined;
  expectedSourceBucketOwner?: string | undefined;
  copySourceIfMatch?: string | undefined;
  copySourceIfNoneMatch?: string | undefined;
  copySourceIfModifiedSince?: number | undefined;
  copySourceIfUnmodifiedSince?: number | undefined;
  metadataDirective?: CopyDirective | undefined;
  contentType?: string | undefined;
  contentEncoding?: string | undefined;
  contentDisposition?: string | undefined;
  contentLanguage?: string | undefined;
  cacheControl?: string | undefined;
  expires?: number | undefined;
  metadata?: readonly Tag[] | undefined;
  tags?: readonly Tag[] | undefined;
  taggingDirective?: CopyDirective | undefined;
  checksumAlgorithm?: ChecksumAlgorithm | undefined;
  ifMatch?: string | undefined;
  ifNoneMatch?: string | undefined;
}

export interface NativeCreateMultipartUploadInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
  contentType?: string | undefined;
  contentEncoding?: string | undefined;
  contentDisposition?: string | undefined;
  contentLanguage?: string | undefined;
  cacheControl?: string | undefined;
  expires?: number | undefined;
  metadata?: readonly Tag[] | undefined;
  tags?: readonly Tag[] | undefined;
  checksumAlgorithm?: ChecksumAlgorithm | undefined;
  checksumType?: ChecksumType | undefined;
}

export interface NativeUploadPartInput {
  bucket: string;
  key: string;
  uploadId: string;
  partNumber: number;
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

export interface NativeUploadPartCopyInput {
  bucket: string;
  key: string;
  uploadId: string;
  partNumber: number;
  copySource: ObjectRef;
  expectedBucketOwner?: string | undefined;
  expectedSourceBucketOwner?: string | undefined;
  copySourceIfMatch?: string | undefined;
  copySourceIfNoneMatch?: string | undefined;
  copySourceIfModifiedSince?: number | undefined;
  copySourceIfUnmodifiedSince?: number | undefined;
  copySourceRange?: string | undefined;
}

export interface NativeCompleteMultipartUploadInput {
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

export interface NativeAbortMultipartUploadInput {
  bucket: string;
  key: string;
  uploadId: string;
  expectedBucketOwner?: string | undefined;
}

export interface NativeListPartsInput {
  bucket: string;
  key: string;
  uploadId: string;
  expectedBucketOwner?: string | undefined;
  partNumberMarker?: number | undefined;
  maxParts?: number | undefined;
}

export interface NativeListMultipartUploadsInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
  prefix?: string | undefined;
  delimiter?: string | undefined;
  keyMarker?: string | undefined;
  uploadIdMarker?: string | undefined;
  maxUploads?: number | undefined;
}

export interface NativeCopyObjectOutput {
  etag: string;
  lastModified: number;
  checksumCrc32?: string | undefined;
  checksumCrc32c?: string | undefined;
  checksumSha1?: string | undefined;
  checksumSha256?: string | undefined;
  checksumCrc64Nvme?: string | undefined;
}

export interface NativeCreateMultipartUploadOutput {
  uploadId: string;
  checksumAlgorithm: ChecksumAlgorithm;
  checksumType: ChecksumType;
}

export interface NativeUploadPartOutput {
  etag: string;
  checksumCrc32?: string | undefined;
  checksumCrc32c?: string | undefined;
  checksumSha1?: string | undefined;
  checksumSha256?: string | undefined;
  checksumCrc64Nvme?: string | undefined;
}

export interface NativeUploadPartCopyOutput {
  etag: string;
  lastModified: number;
  checksumCrc32?: string | undefined;
  checksumCrc32c?: string | undefined;
  checksumSha1?: string | undefined;
  checksumSha256?: string | undefined;
  checksumCrc64Nvme?: string | undefined;
}

export interface NativeCompleteMultipartUploadOutput {
  etag: string;
  checksumType: ChecksumType;
  checksumCrc32?: string | undefined;
  checksumCrc32c?: string | undefined;
  checksumSha1?: string | undefined;
  checksumSha256?: string | undefined;
  checksumCrc64Nvme?: string | undefined;
}

export interface NativePart {
  partNumber: number;
  size: bigint;
  etag: string;
  lastModified: number;
  checksumCrc32?: string | undefined;
  checksumCrc32c?: string | undefined;
  checksumSha1?: string | undefined;
  checksumSha256?: string | undefined;
  checksumCrc64Nvme?: string | undefined;
}

export interface NativeMultipartUpload {
  key: string;
  uploadId: string;
  initiated: number;
  checksumAlgorithm: ChecksumAlgorithm;
  checksumType: ChecksumType;
}

export interface NativeListPartsOutput {
  parts: NativePart[];
  isTruncated: boolean;
  nextPartNumberMarker?: number | undefined;
  checksumAlgorithm: ChecksumAlgorithm;
  checksumType: ChecksumType;
}

export interface NativeListMultipartUploadsOutput {
  uploads: NativeMultipartUpload[];
  commonPrefixes: string[];
  isTruncated: boolean;
  nextKeyMarker?: string | undefined;
  nextUploadIdMarker?: string | undefined;
}
