/** Literal object tag; keys remain case-sensitive and values may be empty. */
export interface Tag {
  readonly key: string;
  readonly value: string;
}

export type ChecksumAlgorithm =
  "CRC32" | "CRC32C" | "CRC64NVME" | "SHA1" | "SHA256";
export type ChecksumMode = "ENABLED";
export type ChecksumType = "FULL_OBJECT" | "COMPOSITE";

/** Mutation evidence on failure; absence never establishes rollback. */
export type MutationOutcome =
  "NotCommitted" | "Committed" | "Unknown" | "Partial";

export interface CreateBucketInput {
  bucket: string;
}

export interface HeadBucketInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export interface GetBucketLocationInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export interface DeleteBucketInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export type CreateBucketOutput = Readonly<Record<string, never>>;
export type DeleteBucketOutput = Readonly<Record<string, never>>;

export interface HeadBucketOutput {
  readonly bucketRegion: string;
}

export interface GetBucketLocationOutput {
  readonly locationConstraint?: string | undefined;
}

/**
 * Publish bytes with optional integrity and destination conditions.
 * Keep mutable source bytes unchanged until the next pull or terminal settlement.
 * contentLength counts bytes; dates are UTC instants and metadata keys are normalized.
 */
export interface PutObjectInput {
  bucket: string;
  key: string;
  body: Uint8Array | AsyncIterable<Uint8Array>;
  expectedBucketOwner?: string | undefined;
  contentLength?: bigint | undefined;
  contentType?: string | undefined;
  contentEncoding?: string | undefined;
  contentDisposition?: string | undefined;
  contentLanguage?: string | undefined;
  cacheControl?: string | undefined;
  expires?: Date | undefined;
  metadata?: Readonly<Record<string, string>> | undefined;
  tags?: readonly Tag[] | undefined;
  checksumAlgorithm?: ChecksumAlgorithm | undefined;
  contentMd5?: string | undefined;
  checksumCrc32?: string | undefined;
  checksumCrc32c?: string | undefined;
  checksumSha1?: string | undefined;
  checksumSha256?: string | undefined;
  checksumCrc64Nvme?: string | undefined;
  ifMatch?: string | undefined;
  ifNoneMatch?: string | undefined;
}

export interface PutObjectOutput {
  readonly etag: string;
  readonly checksumCrc32?: string | undefined;
  readonly checksumCrc32c?: string | undefined;
  readonly checksumSha1?: string | undefined;
  readonly checksumSha256?: string | undefined;
  readonly checksumCrc64Nvme?: string | undefined;
}

/** Select a protected object snapshot; range and partNumber are mutually exclusive. */
export interface GetObjectInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
  checksumMode?: ChecksumMode | undefined;
  range?: string | undefined;
  partNumber?: number | undefined;
  ifMatch?: string | undefined;
  ifNoneMatch?: string | undefined;
  ifModifiedSince?: Date | undefined;
  ifUnmodifiedSince?: Date | undefined;
  responseContentType?: string | undefined;
  responseContentEncoding?: string | undefined;
  responseContentDisposition?: string | undefined;
  responseContentLanguage?: string | undefined;
  responseCacheControl?: string | undefined;
  responseExpires?: Date | undefined;
}

/** Read the same selection facts as GET without acquiring a response body. */
export interface HeadObjectInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
  checksumMode?: ChecksumMode | undefined;
  range?: string | undefined;
  partNumber?: number | undefined;
  ifMatch?: string | undefined;
  ifNoneMatch?: string | undefined;
  ifModifiedSince?: Date | undefined;
  ifUnmodifiedSince?: Date | undefined;
  responseContentType?: string | undefined;
  responseContentEncoding?: string | undefined;
  responseContentDisposition?: string | undefined;
  responseContentLanguage?: string | undefined;
  responseCacheControl?: string | undefined;
  responseExpires?: Date | undefined;
}

export interface DeleteObjectInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
  ifMatch?: string | undefined;
}

/** One literal key and an optional quoted ETag or existence condition. */
export interface ObjectIdentifier {
  key: string;
  etag?: string | undefined;
}

/** Delete up to 1,000 entries independently; quiet suppresses only successful entries. */
export interface DeleteObjectsInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
  objects: readonly ObjectIdentifier[];
  quiet?: boolean | undefined;
}

export interface DeletedObject {
  readonly key: string;
}

export interface DeleteError {
  readonly code: string;
  readonly message: string;
}

export interface DeleteObjectError extends DeleteError {
  readonly key: string;
}

export interface DeleteObjectsOutput {
  readonly deleted: readonly DeletedObject[];
  readonly errors: readonly DeleteObjectError[];
}

/** Progress for the original zero-based input position, including duplicate keys. */
export interface DeleteObjectProgress {
  readonly index: number;
  readonly key: string;
  readonly mutationOutcome: Exclude<MutationOutcome, "Partial">;
  readonly error?: DeleteError | undefined;
}

export interface DeleteObjectsProgress {
  readonly items: readonly DeleteObjectProgress[];
}

export type DeleteObjectOutput = Readonly<Record<string, never>>;

/** Detached selected-object facts; contentLength is a byte count. */
export interface HeadObjectOutput {
  readonly contentLength: bigint;
  readonly etag: string;
  readonly lastModified: Date;
  readonly metadata: Readonly<Record<string, string>>;
  readonly tagCount: number;
  readonly contentType?: string | undefined;
  readonly contentEncoding?: string | undefined;
  readonly contentDisposition?: string | undefined;
  readonly contentLanguage?: string | undefined;
  readonly cacheControl?: string | undefined;
  readonly contentRange?: string | undefined;
  readonly checksumCrc32?: string | undefined;
  readonly checksumCrc32c?: string | undefined;
  readonly checksumSha1?: string | undefined;
  readonly checksumSha256?: string | undefined;
  readonly checksumCrc64Nvme?: string | undefined;
  readonly expires?: Date | undefined;
  readonly partsCount?: number | undefined;
  readonly checksumType?: ChecksumType | undefined;
}

/** Detached facts and one owned cursor; close an unread body explicitly. */
export interface GetObjectOutput extends HeadObjectOutput {
  readonly body: import("./body.cjs").ObjectBody;
}

/** A positive safe-integer byte bound for one cursor read. */
export interface ReadChunkInput {
  maxBytes: number;
}

/** A nonnegative safe-integer collection bound; overflow closes the cursor. */
export interface ReadAllInput {
  maxBytes: number;
}

/** Trusted local administration. Lists contain no signing secrets. */
export interface OwnerInfo {
  readonly accountId: string;
  readonly canonicalId: string;
}

export interface PrincipalInfo {
  readonly principalId: string;
  readonly name: string;
  readonly isEnabled: boolean;
  readonly createdAt: Date;
}

export interface AccessKeyInfo {
  readonly accessKeyId: string;
  readonly principalId: string;
  readonly createdAt: Date;
}

export interface CreateAccessKeyOutput {
  readonly accessKey: AccessKeyInfo;
  readonly secretAccessKey: string;
}

export interface ListPrincipalsOutput {
  readonly principals: readonly PrincipalInfo[];
  readonly nextContinuationToken?: string | undefined;
}

export interface ListAccessKeysOutput {
  readonly accessKeys: readonly AccessKeyInfo[];
  readonly nextContinuationToken?: string | undefined;
}

export interface GetPrincipalPolicyOutput {
  readonly document: string;
}

export interface CreatePrincipalInput {
  name: string;
}

export interface GetPrincipalInput {
  principalId: string;
}

export interface SetPrincipalEnabledInput {
  principalId: string;
  isEnabled: boolean;
}

export interface DeletePrincipalInput {
  principalId: string;
}

export type DeletePrincipalOutput = Readonly<Record<string, never>>;

export interface PutPrincipalPolicyInput {
  principalId: string;
  document: string;
}

export type PutPrincipalPolicyOutput = Readonly<Record<string, never>>;

export interface GetPrincipalPolicyInput {
  principalId: string;
}

export interface DeletePrincipalPolicyInput {
  principalId: string;
}

export type DeletePrincipalPolicyOutput = Readonly<Record<string, never>>;

export interface CreateAccessKeyInput {
  principalId: string;
}

export interface DeleteAccessKeyInput {
  principalId: string;
  accessKeyId: string;
}

export type DeleteAccessKeyOutput = Readonly<Record<string, never>>;

export interface ListPrincipalsInput {
  maxResults?: number | undefined;
  continuationToken?: string | undefined;
}

export interface ListAccessKeysInput {
  principalId: string;
  maxResults?: number | undefined;
  continuationToken?: string | undefined;
}

/** Complete native replacement: all four boolean flags are required. */
export interface PublicAccessBlockConfiguration {
  readonly blockPublicAcls: boolean;
  readonly ignorePublicAcls: boolean;
  readonly blockPublicPolicy: boolean;
  readonly restrictPublicBuckets: boolean;
}

export type ObjectOwnership = "BucketOwnerEnforced";
export type AclPermission = "FULL_CONTROL";

export interface AclOwner {
  readonly canonicalId: string;
}

export interface AclGrant {
  readonly granteeCanonicalId: string;
  readonly permission: AclPermission;
}

/** One ordered rule. Shared Rust validation owns semantic bounds. */
export interface CorsRule {
  readonly id?: string | undefined;
  readonly allowedOrigins: readonly string[];
  readonly allowedMethods: readonly string[];
  readonly allowedHeaders?: readonly string[] | undefined;
  readonly exposeHeaders?: readonly string[] | undefined;
  readonly maxAgeSeconds?: number | undefined;
}

export interface PutBucketCorsInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
  corsRules: readonly CorsRule[];
}

export type PutBucketCorsOutput = Readonly<Record<string, never>>;

export interface GetBucketCorsInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export interface GetBucketCorsOutput {
  readonly corsRules: readonly CorsRule[];
}

export interface DeleteBucketCorsInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export type DeleteBucketCorsOutput = Readonly<Record<string, never>>;

/** Supported lifecycle configuration; every rule requires at least one action. */
export type LifecycleStatus = "Enabled" | "Disabled";

export interface LifecycleFilter {
  readonly prefix?: string | undefined;
}

export interface LifecycleExpiration {
  readonly days: number;
}

export interface AbortIncompleteMultipartUpload {
  readonly daysAfterInitiation: number;
}

export interface LifecycleRule {
  readonly id?: string | undefined;
  readonly status: LifecycleStatus;
  readonly filter: LifecycleFilter;
  readonly expiration?: LifecycleExpiration | undefined;
  readonly abortIncompleteMultipartUpload?:
    AbortIncompleteMultipartUpload | undefined;
}

export interface PutBucketLifecycleConfigurationInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
  lifecycleRules: readonly LifecycleRule[];
}

export type PutBucketLifecycleConfigurationOutput = Readonly<
  Record<string, never>
>;

export interface GetBucketLifecycleConfigurationInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export interface GetBucketLifecycleConfigurationOutput {
  readonly lifecycleRules: readonly LifecycleRule[];
}

export interface DeleteBucketLifecycleInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export type DeleteBucketLifecycleOutput = Readonly<Record<string, never>>;

export interface PutBucketPolicyInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
  document: string;
}

export type PutBucketPolicyOutput = Readonly<Record<string, never>>;

export interface GetBucketPolicyInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export interface GetBucketPolicyOutput {
  readonly document: string;
}

export interface DeleteBucketPolicyInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export type DeleteBucketPolicyOutput = Readonly<Record<string, never>>;

export interface GetBucketPolicyStatusInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export interface GetBucketPolicyStatusOutput {
  readonly isPublic: boolean;
}

export interface PutPublicAccessBlockInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
  configuration: PublicAccessBlockConfiguration;
}

export type PutPublicAccessBlockOutput = Readonly<Record<string, never>>;

export interface GetPublicAccessBlockInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export interface GetPublicAccessBlockOutput {
  readonly configuration: PublicAccessBlockConfiguration;
}

export interface DeletePublicAccessBlockInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export type DeletePublicAccessBlockOutput = Readonly<Record<string, never>>;

export interface PutBucketOwnershipControlsInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
  objectOwnership: ObjectOwnership;
}

export type PutBucketOwnershipControlsOutput = Readonly<Record<string, never>>;

export interface GetBucketOwnershipControlsInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export interface GetBucketOwnershipControlsOutput {
  readonly objectOwnership: ObjectOwnership;
}

export interface GetBucketAclInput {
  bucket: string;
  expectedBucketOwner?: string | undefined;
}

export interface GetBucketAclOutput {
  readonly owner: AclOwner;
  readonly grants: readonly AclGrant[];
}

export interface GetObjectAclInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
}

export interface GetObjectAclOutput {
  readonly owner: AclOwner;
  readonly grants: readonly AclGrant[];
}

/** One live namespace page; filters are literal strings, not paths. */
export interface ListObjectsV2Input {
  bucket: string;
  expectedBucketOwner?: string | undefined;
  prefix?: string | undefined;
  delimiter?: string | undefined;
  maxKeys?: number | undefined;
  continuationToken?: string | undefined;
  startAfter?: string | undefined;
}

/** Detached full-object facts. Size always uses bigint. */
export interface ObjectSummary {
  readonly key: string;
  readonly size: bigint;
  readonly etag: string;
  readonly lastModified: Date;
}

/** Literal grouping prefix; not a filesystem directory. */
export interface CommonPrefix {
  readonly prefix: string;
}

/** Contents may be empty while common prefixes and continuation remain. */
export interface ListObjectsV2Output {
  readonly contents: readonly ObjectSummary[];
  readonly commonPrefixes: readonly CommonPrefix[];
  readonly isTruncated: boolean;
  readonly nextContinuationToken: string | undefined;
}

/** One native bucket page, defaulting to 1,000 entries. */
export interface ListBucketsInput {
  prefix?: string | undefined;
  maxBuckets?: number | undefined;
  continuationToken?: string | undefined;
}

export interface BucketSummary {
  readonly name: string;
  readonly creationDate: Date;
  readonly bucketRegion: string;
}

/** An absent continuation token marks the final page. */
export interface ListBucketsOutput {
  readonly buckets: readonly BucketSummary[];
  readonly continuationToken: string | undefined;
}

/** Owned values for Store.getObjectTagging. */
export interface GetObjectTaggingInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
}

export type GetObjectTaggingOutput = { readonly tags: readonly Tag[] };

/** Owned values for Store.putObjectTagging. */
export interface PutObjectTaggingInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
  tags: readonly Tag[];
}

export type PutObjectTaggingOutput = Readonly<Record<string, never>>;

/** Owned values for Store.deleteObjectTagging. */
export interface DeleteObjectTaggingInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
}

export type DeleteObjectTaggingOutput = Readonly<Record<string, never>>;

export type * from "./multipart-values.cjs";

/** Exact Attributes selector spellings. */
export type ObjectAttribute =
  "ETag" | "Checksum" | "ObjectParts" | "StorageClass" | "ObjectSize";

export interface GetObjectAttributesInput {
  bucket: string;
  key: string;
  expectedBucketOwner?: string | undefined;
  objectAttributes: readonly ObjectAttribute[];
  maxParts?: number | undefined;
  partNumberMarker?: number | undefined;
}

export interface Checksum {
  readonly checksumCrc32?: string | undefined;
  readonly checksumCrc32c?: string | undefined;
  readonly checksumSha1?: string | undefined;
  readonly checksumSha256?: string | undefined;
  readonly checksumCrc64Nvme?: string | undefined;
  readonly checksumType?: ChecksumType | undefined;
}

export interface ObjectPart extends Omit<Checksum, "checksumType"> {
  readonly partNumber: number;
  readonly size: bigint;
}

export interface ObjectParts {
  readonly totalPartsCount: number;
  readonly maxParts: number;
  readonly partNumberMarker: number;
  readonly parts: readonly ObjectPart[];
  readonly isTruncated: boolean;
  readonly nextPartNumberMarker?: number | undefined;
}

export interface GetObjectAttributesOutput {
  readonly objectSize?: bigint | undefined;
  readonly etag?: string | undefined;
  readonly checksum?: Checksum | undefined;
  readonly storageClass?: string | undefined;
  readonly objectParts?: ObjectParts | undefined;
  readonly lastModified: Date;
}
