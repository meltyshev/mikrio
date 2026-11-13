import { createRequire } from "node:module";
import type {
  NativeCopyObjectInput,
  NativeCopyObjectOutput,
  NativeCreateMultipartUploadInput,
  NativeCreateMultipartUploadOutput,
  NativeUploadPartInput,
  NativeUploadPartOutput,
  NativeUploadPartCopyInput,
  NativeUploadPartCopyOutput,
  NativeCompleteMultipartUploadInput,
  NativeCompleteMultipartUploadOutput,
  NativeAbortMultipartUploadInput,
  NativeListPartsInput,
  NativeListPartsOutput,
  NativeListMultipartUploadsInput,
  NativeListMultipartUploadsOutput,
} from "./multipart-native.cjs";
import type {
  GetObjectAttributesInput,
  GetObjectAttributesOutput,
  GetObjectTaggingInput,
  GetObjectTaggingOutput,
  PutObjectTaggingInput,
  DeleteObjectTaggingInput,
  ListObjectsV2Input,
  ListBucketsInput,
  DeleteObjectsInput,
  DeleteObjectsOutput,
  OwnerInfo,
  PrincipalInfo,
  AccessKeyInfo,
  GetPrincipalPolicyOutput,
  CreatePrincipalInput,
  GetPrincipalInput,
  SetPrincipalEnabledInput,
  DeletePrincipalInput,
  PutPrincipalPolicyInput,
  GetPrincipalPolicyInput,
  DeletePrincipalPolicyInput,
  CreateAccessKeyInput,
  DeleteAccessKeyInput,
  ListPrincipalsInput,
  ListAccessKeysInput,
} from "./values.cjs";
import { nativePackageName, usesGlibc } from "./target.cjs";
import type {
  PutBucketCorsInput,
  PutBucketLifecycleConfigurationInput,
  GetBucketCorsInput,
  GetBucketLifecycleConfigurationInput,
  GetBucketCorsOutput,
  GetBucketLifecycleConfigurationOutput,
  DeleteBucketCorsInput,
  DeleteBucketLifecycleInput,
  PutBucketPolicyInput,
  GetBucketPolicyInput,
  GetBucketPolicyOutput,
  DeleteBucketPolicyInput,
  GetBucketPolicyStatusInput,
  GetBucketPolicyStatusOutput,
  PutPublicAccessBlockInput,
  GetPublicAccessBlockInput,
  GetPublicAccessBlockOutput,
  DeletePublicAccessBlockInput,
  PutBucketOwnershipControlsInput,
  GetBucketOwnershipControlsInput,
  GetBucketOwnershipControlsOutput,
  GetBucketAclInput,
  GetBucketAclOutput,
  GetObjectAclInput,
  GetObjectAclOutput,
} from "./values.cjs";
import type {
  GetObjectInput,
  HeadObjectOutput,
  PutObjectInput,
  PutObjectOutput,
  Tag,
} from "./values.cjs";
import type { StoreInfo } from "./index.cjs";
import type { Credentials, ListenOptions } from "./listener.cjs";

export interface NativeServer {
  readonly info: Credentials & { endpoint: string; region: string };
  closed(): Promise<void>;
  close(): Promise<void>;
}

export interface NativeCancellation {
  cancel(): void;
}

export interface NativeDiagnostics {
  sameQueue(other: NativeDiagnostics): boolean;
  isClosed(): boolean;
  take(): string | null;
  observerFailed(): void;
}

export interface NativeRequest {
  write(bytes: Uint8Array): void;
  finish(): void;
  fail(): void;
}

export interface NativeUpload {
  release(): void;
  next(): Promise<NativeRequest | null>;
}

export interface NativeBody {
  readChunk(
    maxBytes: number,
    cancellation: NativeCancellation,
  ): Promise<Uint8Array | null>;
  readAll(
    maxBytes: number,
    cancellation: NativeCancellation,
  ): Promise<Uint8Array>;
  cancel(): void;
  close(): Promise<void>;
}

export type NativePutInput = Omit<
  PutObjectInput,
  "body" | "expires" | "metadata"
> & {
  expires?: number | undefined;
  metadata?: readonly Tag[] | undefined;
};

export type NativeReadInput = Omit<
  GetObjectInput,
  "ifModifiedSince" | "ifUnmodifiedSince" | "responseExpires"
> & {
  responseExpires?: number | undefined;
  ifModifiedSince?: number | undefined;
  ifUnmodifiedSince?: number | undefined;
};

export type NativeReadOutput = Omit<
  HeadObjectOutput,
  "lastModified" | "expires" | "metadata"
> & {
  lastModified: number;
  expires?: number | undefined;
  metadata: readonly Tag[];
};

export type NativePrincipalInfo = Omit<PrincipalInfo, "createdAt"> & {
  createdAt: number;
};

export type NativeAccessKeyInfo = Omit<AccessKeyInfo, "createdAt"> & {
  createdAt: number;
};

export interface NativeCreateAccessKeyOutput {
  accessKey: NativeAccessKeyInfo;
  secretAccessKey: string;
}

export interface NativeListPrincipalsOutput {
  principals: NativePrincipalInfo[];
  nextContinuationToken?: string | undefined;
}

export interface NativeListAccessKeysOutput {
  accessKeys: NativeAccessKeyInfo[];
  nextContinuationToken?: string | undefined;
}

export interface NativeStore {
  copyObject(
    input: NativeCopyObjectInput,
    cancellation: NativeCancellation,
  ): Promise<NativeCopyObjectOutput>;
  createMultipartUpload(
    input: NativeCreateMultipartUploadInput,
    cancellation: NativeCancellation,
  ): Promise<NativeCreateMultipartUploadOutput>;
  uploadPart(
    input: NativeUploadPartInput,
    upload: NativeUpload,
    cancellation: NativeCancellation,
  ): Promise<NativeUploadPartOutput>;
  uploadPartCopy(
    input: NativeUploadPartCopyInput,
    cancellation: NativeCancellation,
  ): Promise<NativeUploadPartCopyOutput>;
  completeMultipartUpload(
    input: NativeCompleteMultipartUploadInput,
    cancellation: NativeCancellation,
  ): Promise<NativeCompleteMultipartUploadOutput>;
  abortMultipartUpload(
    input: NativeAbortMultipartUploadInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  listParts(
    input: NativeListPartsInput,
    cancellation: NativeCancellation,
  ): Promise<NativeListPartsOutput>;
  listMultipartUploads(
    input: NativeListMultipartUploadsInput,
    cancellation: NativeCancellation,
  ): Promise<NativeListMultipartUploadsOutput>;
  getObjectAttributes(
    input: GetObjectAttributesInput,
    cancellation: NativeCancellation,
  ): Promise<
    Omit<GetObjectAttributesOutput, "lastModified"> & { lastModified: number }
  >;
  getObjectTagging(
    input: GetObjectTaggingInput,
    cancellation: NativeCancellation,
  ): Promise<GetObjectTaggingOutput>;
  putObjectTagging(
    input: PutObjectTaggingInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  deleteObjectTagging(
    input: DeleteObjectTaggingInput,
    cancellation: NativeCancellation,
  ): Promise<void>;

  listBuckets(
    input: ListBucketsInput,
    cancellation: NativeCancellation,
  ): Promise<{
    buckets: { name: string; creationDate: number; bucketRegion: string }[];
    continuationToken: string | undefined;
  }>;
  listObjectsV2(
    input: ListObjectsV2Input,
    cancellation: NativeCancellation,
  ): Promise<{
    contents: {
      key: string;
      size: bigint;
      etag: string;
      lastModified: number;
    }[];
    commonPrefixes: string[];
    isTruncated: boolean;
    nextContinuationToken: string | undefined;
  }>;
  putBucketCors(
    input: PutBucketCorsInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  getBucketCors(
    input: GetBucketCorsInput,
    cancellation: NativeCancellation,
  ): Promise<GetBucketCorsOutput>;
  deleteBucketCors(
    input: DeleteBucketCorsInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  putBucketLifecycleConfiguration(
    input: PutBucketLifecycleConfigurationInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  getBucketLifecycleConfiguration(
    input: GetBucketLifecycleConfigurationInput,
    cancellation: NativeCancellation,
  ): Promise<GetBucketLifecycleConfigurationOutput>;
  deleteBucketLifecycle(
    input: DeleteBucketLifecycleInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  putBucketPolicy(
    input: PutBucketPolicyInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  getBucketPolicy(
    input: GetBucketPolicyInput,
    cancellation: NativeCancellation,
  ): Promise<GetBucketPolicyOutput>;
  deleteBucketPolicy(
    input: DeleteBucketPolicyInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  getBucketPolicyStatus(
    input: GetBucketPolicyStatusInput,
    cancellation: NativeCancellation,
  ): Promise<GetBucketPolicyStatusOutput>;
  putPublicAccessBlock(
    input: PutPublicAccessBlockInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  getPublicAccessBlock(
    input: GetPublicAccessBlockInput,
    cancellation: NativeCancellation,
  ): Promise<GetPublicAccessBlockOutput>;
  deletePublicAccessBlock(
    input: DeletePublicAccessBlockInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  putBucketOwnershipControls(
    input: PutBucketOwnershipControlsInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  getBucketOwnershipControls(
    input: GetBucketOwnershipControlsInput,
    cancellation: NativeCancellation,
  ): Promise<GetBucketOwnershipControlsOutput>;
  getBucketAcl(
    input: GetBucketAclInput,
    cancellation: NativeCancellation,
  ): Promise<GetBucketAclOutput>;
  getObjectAcl(
    input: GetObjectAclInput,
    cancellation: NativeCancellation,
  ): Promise<GetObjectAclOutput>;

  listen(
    input: ListenOptions,
    cancellation: NativeCancellation,
  ): Promise<NativeServer>;
  getOwnerInfo(cancellation: NativeCancellation): Promise<OwnerInfo>;
  createPrincipal(
    input: CreatePrincipalInput,
    cancellation: NativeCancellation,
  ): Promise<NativePrincipalInfo>;
  getPrincipal(
    input: GetPrincipalInput,
    cancellation: NativeCancellation,
  ): Promise<NativePrincipalInfo>;
  setPrincipalEnabled(
    input: SetPrincipalEnabledInput,
    cancellation: NativeCancellation,
  ): Promise<NativePrincipalInfo>;
  deletePrincipal(
    input: DeletePrincipalInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  putPrincipalPolicy(
    input: PutPrincipalPolicyInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  getPrincipalPolicy(
    input: GetPrincipalPolicyInput,
    cancellation: NativeCancellation,
  ): Promise<GetPrincipalPolicyOutput>;
  deletePrincipalPolicy(
    input: DeletePrincipalPolicyInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  createAccessKey(
    input: CreateAccessKeyInput,
    cancellation: NativeCancellation,
  ): Promise<NativeCreateAccessKeyOutput>;
  deleteAccessKey(
    input: DeleteAccessKeyInput,
    cancellation: NativeCancellation,
  ): Promise<void>;
  listPrincipals(
    input: ListPrincipalsInput,
    cancellation: NativeCancellation,
  ): Promise<NativeListPrincipalsOutput>;
  listAccessKeys(
    input: ListAccessKeysInput,
    cancellation: NativeCancellation,
  ): Promise<NativeListAccessKeysOutput>;

  readonly info: StoreInfo;
  readonly diagnostics: NativeDiagnostics;
  createBucket(bucket: string, cancellation: NativeCancellation): Promise<void>;
  headBucket(
    input: { bucket: string; expectedBucketOwner?: string | undefined },
    cancellation: NativeCancellation,
  ): Promise<string>;
  getBucketLocation(
    input: { bucket: string; expectedBucketOwner?: string | undefined },
    cancellation: NativeCancellation,
  ): Promise<string | null>;
  deleteBucket(
    input: { bucket: string; expectedBucketOwner?: string | undefined },
    cancellation: NativeCancellation,
  ): Promise<void>;
  putObject(
    input: NativePutInput,
    upload: NativeUpload,
    cancellation: NativeCancellation,
  ): Promise<PutObjectOutput>;
  getObject(
    input: NativeReadInput,
    cancellation: NativeCancellation,
  ): Promise<{ facts: NativeReadOutput; body: NativeBody }>;
  headObject(
    input: NativeReadInput,
    cancellation: NativeCancellation,
  ): Promise<NativeReadOutput>;
  deleteObjects(
    input: DeleteObjectsInput,
    cancellation: NativeCancellation,
  ): Promise<DeleteObjectsOutput>;
  deleteObject(
    input: {
      bucket: string;
      key: string;
      expectedBucketOwner?: string | undefined;
      ifMatch?: string | undefined;
    },
    cancellation: NativeCancellation,
  ): Promise<void>;
  close(): Promise<void>;
}

interface NativeAddon {
  NativeCancellation: new () => NativeCancellation;
  NativeUpload: new () => NativeUpload;
  open(
    directory: string,
    region: string | undefined,
    createIfMissing: boolean,
    cancellation: NativeCancellation,
  ): Promise<NativeStore>;
}

let addon: NativeAddon | undefined;

export function loadNative(): NativeAddon {
  if (addon !== undefined) {
    return addon;
  }

  const require = createRequire(__filename);
  const name = nativePackageName(process.platform, process.arch, usesGlibc());
  const expected: unknown = require("../package.json");
  let installed: unknown;

  try {
    installed = require(name + "/package.json");
  } catch (cause) {
    throw new Error(
      `Missing mikrio native package ${name}; install its matching optional dependency`,
      { cause },
    );
  }

  const matchesPackage =
    isPackage(expected) &&
    expected.name === "@mikrio/node" &&
    isPackage(installed) &&
    installed.name === name &&
    installed.version === expected.version;

  if (!matchesPackage) {
    throw new Error(`Mismatched mikrio native package: ${name}`);
  }

  const candidate: unknown = require(name);

  if (!isNativeAddon(candidate)) {
    throw new Error("Invalid mikrio native artifact");
  }

  addon = candidate;
  return candidate;
}

function isPackage(value: unknown): value is { name: string; version: string } {
  return (
    typeof value === "object" &&
    value !== null &&
    "name" in value &&
    typeof value.name === "string" &&
    "version" in value &&
    typeof value.version === "string"
  );
}

function isNativeAddon(value: unknown): value is NativeAddon {
  return (
    typeof value === "object" &&
    value !== null &&
    "open" in value &&
    typeof value.open === "function" &&
    "NativeCancellation" in value &&
    typeof value.NativeCancellation === "function" &&
    "NativeUpload" in value &&
    typeof value.NativeUpload === "function"
  );
}
