# S3 compatibility

mikrio implements a local, general-purpose S3 subset. It is not a complete AWS
S3, IAM, or STS replacement. Configure clients with the listener endpoint,
persisted region, explicit credentials, and path-style addressing. Use Signature
Version 4.

The operations below are implemented. SDK versions and transfer helpers may have
additional requirements. Remote browser deployments and the full high-level AWS
CLI transfer matrix have not been verified for 0.1.0.

## Operations

- **Buckets:** CreateBucket, ListBuckets, HeadBucket, DeleteBucket,
  GetBucketLocation.
- **Objects:** PutObject, GetObject, HeadObject, DeleteObject, DeleteObjects,
  CopyObject, ListObjectsV2.
- **Multipart:** CreateMultipartUpload, UploadPart, UploadPartCopy, ListParts,
  ListMultipartUploads, CompleteMultipartUpload, AbortMultipartUpload.
- **Object attributes:** GetObjectAttributes.
- **Object tags:** PutObjectTagging, GetObjectTagging, DeleteObjectTagging.
- **CORS:** PutBucketCors, GetBucketCors, DeleteBucketCors.
- **Lifecycle:** PutBucketLifecycleConfiguration,
  GetBucketLifecycleConfiguration, DeleteBucketLifecycle.
- **Bucket policies:** PutBucketPolicy, GetBucketPolicy, DeleteBucketPolicy,
  GetBucketPolicyStatus.
- **Public access:** PutPublicAccessBlock, GetPublicAccessBlock,
  DeletePublicAccessBlock.
- **Ownership:** PutBucketOwnershipControls, GetBucketOwnershipControls,
  GetBucketAcl, GetObjectAcl.
- **Signed forms:** PostObject (HTTP only).

Native bindings expose the same 40 storage operations; signed form POST uses the
existing PUT worker and has no additional native method. Local principal,
policy, and key administration is described in [the API guide](api.md).

## Object behavior

PUT replaces the current object unless a supported condition prevents it.
Destination `If-Match` and `If-None-Match` apply atomically. DELETE supports an
ETag condition; unconditional deletion of an absent key in an existing bucket
succeeds. Batch deletion reports each input position independently.

GET and HEAD support read conditions, a single byte range, completed-object part
selection, checksum mode, expected-owner assertions, and the six signed response
overrides. HEAD never returns a body. Range and part selection are distinct
requests; unsupported combinations are rejected. Body metadata belongs to the
selected generation, including while another writer replaces the key.

COPY supports source read conditions, destination write conditions, and COPY or
REPLACE metadata/tag directives. Part copy reads an existing local object with
optional closed-range selection. A copy source is not an arbitrary URL to fetch.
Clients must inspect the complete response, including embedded errors.

Multipart initiation persists an upload without publishing an object. Parts are
durable, replaceable by part number, and independently validated. Completion
publishes the selected ordered parts as one object. Failed conditions, changed
parts, abort, and competing completion are handled at the publication boundary.
GetObjectAttributes reports selected metadata and completed-part groups.

Supported integrity fields include Content-MD5 and flexible CRC32, CRC32C,
CRC64NVME, SHA1, and SHA256 checksums subject to the operation's full-object or
composite rules. ETags are protocol values, not universal full-object hashes or
unique mutation identifiers. Ordinary reads do not rehash every stored payload.

## Listings, tags, and retention

ListObjectsV2 supports prefixes, delimiters, start-after, bounded pages, and
continuation tokens. Ordering is bytewise. Tokens are scoped to their store,
bucket, and filter; listings across pages are live views, not a frozen snapshot.
ListBuckets and multipart listings have their own pagination fields.

Tag replacement is atomic for the current object and has separate permissions
from reading or writing its body. Tags are case-sensitive, replace the whole
set, and do not change the payload ETag. Tag writes do not support If-Match
conditions; applications must coordinate them with object replacement and their
own state.

Tag keys beginning with any ASCII case variant of `aws:` are reserved. This rule
applies to stored tags as well as new requests. Tag values preserve case and may
contain that prefix.

Lifecycle supports Enabled/Disabled prefix rules with expiration days,
incomplete-multipart abort days, or both. It does not support arbitrary S3
filters, storage-class transitions, or versioned-object actions. Reopening a
store resumes retention using original ages.

For both actions, mikrio adds the configured number of 24-hour days to the
stored object age anchor or upload initiation time, then selects the first UTC
midnight strictly after that instant. This establishes eligibility; bounded
maintenance and active readers can delay cleanup. Changing a rule does not reset
the object's or upload's age.

## Access and browsers

New buckets block public access. Identity policies and bucket policies use the
supported local policy language; unsupported actions, operators, and conditions
are rejected atomically. Removing Block Public Access does not itself grant
access. Policy status describes whether a policy is public, not whether other
controls permit a particular request.

Ownership is BucketOwnerEnforced. ACL reads return owner-only representations;
mutable ACL authorization and other ownership modes are unsupported. Invalid
supplied authentication never falls back to anonymous access.

CORS controls browser access to responses and preflight handling; it grants no
S3 permission. Preflights check bucket CORS rules independently of URL
signatures; the actual storage request still requires authorization. Use
explicit trusted HTTPS origins and a reverse proxy that preserves signed request
components.

Signed multipart/form-data POST supports SigV4 policy conditions, bounded file
streaming, supported metadata/tags/checksums, and success status or redirect
responses. It performs unconditional replacement. A signed policy does not
reserve the key or prevent reuse; applications reconcile late uploads and lost
responses. The [attachment example](../examples/node/attachments/README.md)
shows a permanent-key workflow.

## Unsupported features

- Versioning, object lock, retention legal holds, replication, and event
  delivery.
- Engine-managed encryption at rest, SSE-S3, SSE-KMS, and SSE-C.
- ListObjects V1, bucket tagging, object annotations, and additional bucket
  APIs.
- Mutable ACLs, cross-account delegation, IAM provisioning, roles, and STS
  tokens.
- Virtual-hosted bucket addressing, directory buckets, and a remote storage
  backend.

Recognized unsupported semantics fail explicitly rather than being silently
accepted. Native filesystem access remains administrative authority, outside the
HTTP policy boundary.
