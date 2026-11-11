from collections.abc import Iterable, Mapping
from datetime import datetime
from os import PathLike
from types import TracebackType
from typing import Literal, Protocol, Self, final

__all__ = [
    "Checksum",
    "ObjectPart",
    "ObjectParts",
    "GetObjectAttributesOutput",
    "ObjectRef",
    "CompletedPart",
    "CopyObjectOutput",
    "CreateMultipartUploadOutput",
    "UploadPartOutput",
    "UploadPartCopyOutput",
    "CompleteMultipartUploadOutput",
    "AbortMultipartUploadOutput",
    "PartSummary",
    "MultipartUploadSummary",
    "ListPartsOutput",
    "ListMultipartUploadsOutput",
    "GetObjectTaggingOutput",
    "PutObjectTaggingOutput",
    "DeleteObjectTaggingOutput",
    "ObjectIdentifier",
    "DeletedObject",
    "DeleteError",
    "DeleteObjectError",
    "DeleteObjectsOutput",
    "DeleteObjectProgress",
    "DeleteObjectsProgress",
    "ObjectSummary",
    "CommonPrefix",
    "ListObjectsV2Output",
    "ListBucketsOutput",
    "BucketSummary",
    "CorsRule",
    "LifecycleFilter",
    "LifecycleExpiration",
    "AbortIncompleteMultipartUpload",
    "LifecycleRule",
    "PutBucketLifecycleConfigurationOutput",
    "GetBucketLifecycleConfigurationOutput",
    "DeleteBucketLifecycleOutput",
    "PutBucketCorsOutput",
    "GetBucketCorsOutput",
    "DeleteBucketCorsOutput",
    "PutBucketPolicyOutput",
    "GetBucketPolicyOutput",
    "DeleteBucketPolicyOutput",
    "GetBucketPolicyStatusOutput",
    "PutPublicAccessBlockOutput",
    "GetPublicAccessBlockOutput",
    "DeletePublicAccessBlockOutput",
    "PutBucketOwnershipControlsOutput",
    "GetBucketOwnershipControlsOutput",
    "GetBucketAclOutput",
    "GetObjectAclOutput",
    "PublicAccessBlockConfiguration",
    "AclOwner",
    "AclGrant",
    "Credentials",
    "Server",
    "OwnerInfo",
    "PrincipalInfo",
    "AccessKeyInfo",
    "CreateAccessKeyOutput",
    "ListPrincipalsOutput",
    "ListAccessKeysOutput",
    "GetPrincipalPolicyOutput",
    "PutPrincipalPolicyOutput",
    "DeletePrincipalPolicyOutput",
    "DeleteAccessKeyOutput",
    "DeletePrincipalOutput",
    "BinaryReader",
    "CreateBucketOutput",
    "DeleteBucketOutput",
    "DeleteObjectOutput",
    "GetBucketLocationOutput",
    "GetObjectOutput",
    "HeadBucketOutput",
    "HeadObjectOutput",
    "ObjectBody",
    "PutObjectOutput",
    "StorageError",
    "Store",
    "StoreInfo",
    "Tag",
]

@final
class ObjectIdentifier:
    def __new__(cls, *, key: str, etag: str | None = None) -> Self: ...
    @property
    def key(self) -> str: ...
    @property
    def etag(self) -> str | None: ...

@final
class DeletedObject:
    @property
    def key(self) -> str: ...

@final
class DeleteError:
    @property
    def code(self) -> str: ...
    @property
    def message(self) -> str: ...

@final
class DeleteObjectError:
    @property
    def key(self) -> str: ...
    @property
    def code(self) -> str: ...
    @property
    def message(self) -> str: ...

@final
class DeleteObjectsOutput:
    @property
    def deleted(self) -> tuple[DeletedObject, ...]: ...
    @property
    def errors(self) -> tuple[DeleteObjectError, ...]: ...

@final
class DeleteObjectProgress:
    @property
    def index(self) -> int: ...
    @property
    def key(self) -> str: ...
    @property
    def mutation_outcome(self) -> Literal["NotCommitted", "Committed", "Unknown"]: ...
    @property
    def error(self) -> DeleteError | None: ...

@final
class DeleteObjectsProgress:
    @property
    def items(self) -> tuple[DeleteObjectProgress, ...]: ...

@final
class BucketSummary:
    @property
    def name(self) -> str: ...
    @property
    def creation_date(self) -> datetime: ...
    @property
    def bucket_region(self) -> str: ...

@final
class ListBucketsOutput:
    @property
    def buckets(self) -> tuple[BucketSummary, ...]: ...
    @property
    def continuation_token(self) -> str | None: ...

@final
class ObjectSummary:
    @property
    def key(self) -> str: ...
    @property
    def size(self) -> int: ...
    @property
    def etag(self) -> str: ...
    @property
    def last_modified(self) -> datetime: ...

@final
class CommonPrefix:
    @property
    def prefix(self) -> str: ...

@final
class ListObjectsV2Output:
    @property
    def contents(self) -> tuple[ObjectSummary, ...]: ...
    @property
    def common_prefixes(self) -> tuple[CommonPrefix, ...]: ...
    @property
    def is_truncated(self) -> bool: ...
    @property
    def next_continuation_token(self) -> str | None: ...

@final
class LifecycleFilter:
    def __new__(cls, *, prefix: str | None = None) -> Self: ...
    @property
    def prefix(self) -> str | None: ...

@final
class LifecycleExpiration:
    def __new__(cls, *, days: int) -> Self: ...
    @property
    def days(self) -> int: ...

@final
class AbortIncompleteMultipartUpload:
    def __new__(cls, *, days_after_initiation: int) -> Self: ...
    @property
    def days_after_initiation(self) -> int: ...

@final
class LifecycleRule:
    def __new__(
        cls,
        *,
        status: Literal["Enabled", "Disabled"],
        filter: LifecycleFilter,
        id: str | None = None,
        expiration: LifecycleExpiration | None = None,
        abort_incomplete_multipart_upload: AbortIncompleteMultipartUpload | None = None,
    ) -> Self: ...
    @property
    def id(self) -> str | None: ...
    @property
    def status(self) -> Literal["Enabled", "Disabled"]: ...
    @property
    def filter(self) -> LifecycleFilter: ...
    @property
    def expiration(self) -> LifecycleExpiration | None: ...
    @property
    def abort_incomplete_multipart_upload(
        self,
    ) -> AbortIncompleteMultipartUpload | None: ...

@final
class PutBucketLifecycleConfigurationOutput: ...

@final
class GetBucketLifecycleConfigurationOutput:
    @property
    def lifecycle_rules(self) -> tuple[LifecycleRule, ...]: ...

@final
class DeleteBucketLifecycleOutput: ...

@final
class CorsRule:
    def __new__(
        cls,
        *,
        allowed_origins: Iterable[str],
        allowed_methods: Iterable[str],
        id: str | None = None,
        allowed_headers: Iterable[str] | None = None,
        expose_headers: Iterable[str] | None = None,
        max_age_seconds: int | None = None,
    ) -> Self: ...
    @property
    def id(self) -> str | None: ...
    @property
    def allowed_origins(self) -> tuple[str, ...]: ...
    @property
    def allowed_methods(self) -> tuple[str, ...]: ...
    @property
    def allowed_headers(self) -> tuple[str, ...] | None: ...
    @property
    def expose_headers(self) -> tuple[str, ...] | None: ...
    @property
    def max_age_seconds(self) -> int | None: ...

@final
class PutBucketCorsOutput: ...

@final
class GetBucketCorsOutput:
    @property
    def cors_rules(self) -> tuple[CorsRule, ...]: ...

@final
class DeleteBucketCorsOutput: ...

@final
class PublicAccessBlockConfiguration:
    def __new__(
        cls,
        *,
        block_public_acls: bool,
        ignore_public_acls: bool,
        block_public_policy: bool,
        restrict_public_buckets: bool,
    ) -> Self: ...
    @property
    def block_public_acls(self) -> bool: ...
    @property
    def ignore_public_acls(self) -> bool: ...
    @property
    def block_public_policy(self) -> bool: ...
    @property
    def restrict_public_buckets(self) -> bool: ...

@final
class AclOwner:
    @property
    def canonical_id(self) -> str: ...

@final
class AclGrant:
    @property
    def grantee_canonical_id(self) -> str: ...
    @property
    def permission(self) -> Literal["FULL_CONTROL"]: ...

@final
class PutBucketPolicyOutput: ...

@final
class GetBucketPolicyOutput:
    @property
    def document(self) -> str: ...

@final
class DeleteBucketPolicyOutput: ...

@final
class GetBucketPolicyStatusOutput:
    @property
    def is_public(self) -> bool: ...

@final
class PutPublicAccessBlockOutput: ...

@final
class GetPublicAccessBlockOutput:
    @property
    def configuration(self) -> PublicAccessBlockConfiguration: ...

@final
class DeletePublicAccessBlockOutput: ...

@final
class PutBucketOwnershipControlsOutput: ...

@final
class GetBucketOwnershipControlsOutput:
    @property
    def object_ownership(self) -> Literal["BucketOwnerEnforced"]: ...

@final
class GetBucketAclOutput:
    @property
    def owner(self) -> AclOwner: ...
    @property
    def grants(self) -> tuple[AclGrant, ...]: ...

@final
class GetObjectAclOutput:
    @property
    def owner(self) -> AclOwner: ...
    @property
    def grants(self) -> tuple[AclGrant, ...]: ...

class StorageError(Exception):
    code: str
    mutation_outcome: Literal["NotCommitted", "Committed", "Unknown", "Partial"] | None
    partial_result: DeleteObjectsProgress | None

@final
class StoreInfo:
    @property
    def store_id(self) -> str: ...
    @property
    def format_version(self) -> int: ...
    @property
    def directory(self) -> str: ...
    @property
    def region(self) -> str: ...

@final
class Credentials:
    def __new__(cls, *, access_key_id: str, secret_access_key: str) -> Credentials: ...
    @property
    def access_key_id(self) -> str: ...
    @property
    def secret_access_key(self) -> str: ...

@final
class Server:
    @property
    def endpoint(self) -> str: ...
    @property
    def region(self) -> str: ...
    @property
    def credentials(self) -> Credentials: ...
    def close(self) -> None: ...
    def __enter__(self) -> Self: ...
    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_value: BaseException | None,
        traceback: TracebackType | None,
        /,
    ) -> Literal[False]: ...

@final
class Store:
    def copy_object(
        self,
        *,
        bucket: str,
        key: str,
        copy_source: ObjectRef,
        expected_bucket_owner: str | None = None,
        expected_source_bucket_owner: str | None = None,
        copy_source_if_match: str | None = None,
        copy_source_if_none_match: str | None = None,
        copy_source_if_modified_since: datetime | None = None,
        copy_source_if_unmodified_since: datetime | None = None,
        metadata_directive: Literal["COPY", "REPLACE"] | None = None,
        content_type: str | None = None,
        content_encoding: str | None = None,
        content_disposition: str | None = None,
        content_language: str | None = None,
        cache_control: str | None = None,
        expires: datetime | None = None,
        metadata: Mapping[str, str] | None = None,
        tags: Iterable[Tag] | None = None,
        tagging_directive: Literal["COPY", "REPLACE"] | None = None,
        checksum_algorithm: Literal["CRC32", "CRC32C", "CRC64NVME", "SHA1", "SHA256"]
        | None = None,
        if_match: str | None = None,
        if_none_match: str | None = None,
    ) -> CopyObjectOutput: ...
    def create_multipart_upload(
        self,
        *,
        bucket: str,
        key: str,
        expected_bucket_owner: str | None = None,
        content_type: str | None = None,
        content_encoding: str | None = None,
        content_disposition: str | None = None,
        content_language: str | None = None,
        cache_control: str | None = None,
        expires: datetime | None = None,
        metadata: Mapping[str, str] | None = None,
        tags: Iterable[Tag] | None = None,
        checksum_algorithm: Literal["CRC32", "CRC32C", "CRC64NVME", "SHA1", "SHA256"]
        | None = None,
        checksum_type: Literal["FULL_OBJECT", "COMPOSITE"] | None = None,
    ) -> CreateMultipartUploadOutput: ...
    def upload_part(
        self,
        *,
        bucket: str,
        key: str,
        upload_id: str,
        part_number: int,
        body: bytes | BinaryReader | Iterable[bytes],
        expected_bucket_owner: str | None = None,
        content_length: int | None = None,
        checksum_algorithm: Literal["CRC32", "CRC32C", "CRC64NVME", "SHA1", "SHA256"]
        | None = None,
        content_md5: str | None = None,
        checksum_crc32: str | None = None,
        checksum_crc32c: str | None = None,
        checksum_sha1: str | None = None,
        checksum_sha256: str | None = None,
        checksum_crc64_nvme: str | None = None,
    ) -> UploadPartOutput: ...
    def upload_part_copy(
        self,
        *,
        bucket: str,
        key: str,
        upload_id: str,
        part_number: int,
        copy_source: ObjectRef,
        expected_bucket_owner: str | None = None,
        expected_source_bucket_owner: str | None = None,
        copy_source_if_match: str | None = None,
        copy_source_if_none_match: str | None = None,
        copy_source_if_modified_since: datetime | None = None,
        copy_source_if_unmodified_since: datetime | None = None,
        copy_source_range: str | None = None,
    ) -> UploadPartCopyOutput: ...
    def complete_multipart_upload(
        self,
        *,
        bucket: str,
        key: str,
        upload_id: str,
        parts: Iterable[CompletedPart],
        expected_bucket_owner: str | None = None,
        if_match: str | None = None,
        if_none_match: str | None = None,
        content_length: int | None = None,
        checksum_type: Literal["FULL_OBJECT", "COMPOSITE"] | None = None,
        checksum_crc32: str | None = None,
        checksum_crc32c: str | None = None,
        checksum_sha1: str | None = None,
        checksum_sha256: str | None = None,
        checksum_crc64_nvme: str | None = None,
    ) -> CompleteMultipartUploadOutput: ...
    def abort_multipart_upload(
        self,
        *,
        bucket: str,
        key: str,
        upload_id: str,
        expected_bucket_owner: str | None = None,
    ) -> AbortMultipartUploadOutput: ...
    def list_parts(
        self,
        *,
        bucket: str,
        key: str,
        upload_id: str,
        expected_bucket_owner: str | None = None,
        part_number_marker: int | None = None,
        max_parts: int | None = None,
    ) -> ListPartsOutput: ...
    def list_multipart_uploads(
        self,
        *,
        bucket: str,
        expected_bucket_owner: str | None = None,
        prefix: str | None = None,
        delimiter: str | None = None,
        key_marker: str | None = None,
        upload_id_marker: str | None = None,
        max_uploads: int | None = None,
    ) -> ListMultipartUploadsOutput: ...
    @staticmethod
    def open(
        *,
        directory: str | PathLike[str],
        region: str | None = None,
        create_if_missing: bool = True,
    ) -> Store: ...
    @property
    def info(self) -> StoreInfo: ...
    def listen(
        self,
        *,
        host: str | None = None,
        port: int | None = None,
        public_endpoint: str | None = None,
        credentials: Credentials | None = None,
    ) -> Server: ...
    def get_owner_info(self) -> OwnerInfo: ...
    def create_principal(self, *, name: str) -> PrincipalInfo: ...
    def get_principal(self, *, principal_id: str) -> PrincipalInfo: ...
    def set_principal_enabled(
        self, *, principal_id: str, is_enabled: bool
    ) -> PrincipalInfo: ...
    def delete_principal(self, *, principal_id: str) -> DeletePrincipalOutput: ...
    def put_principal_policy(
        self, *, principal_id: str, document: str
    ) -> PutPrincipalPolicyOutput: ...
    def get_principal_policy(
        self, *, principal_id: str
    ) -> GetPrincipalPolicyOutput: ...
    def delete_principal_policy(
        self, *, principal_id: str
    ) -> DeletePrincipalPolicyOutput: ...
    def create_access_key(self, *, principal_id: str) -> CreateAccessKeyOutput: ...
    def delete_access_key(
        self, *, principal_id: str, access_key_id: str
    ) -> DeleteAccessKeyOutput: ...
    def list_principals(
        self, *, max_results: int | None = None, continuation_token: str | None = None
    ) -> ListPrincipalsOutput: ...
    def list_access_keys(
        self,
        *,
        principal_id: str,
        max_results: int | None = None,
        continuation_token: str | None = None,
    ) -> ListAccessKeysOutput: ...
    def list_buckets(
        self,
        *,
        prefix: str | None = None,
        max_buckets: int | None = None,
        continuation_token: str | None = None,
    ) -> ListBucketsOutput: ...
    def list_objects_v2(
        self,
        *,
        bucket: str,
        expected_bucket_owner: str | None = None,
        prefix: str | None = None,
        delimiter: str | None = None,
        max_keys: int | None = None,
        continuation_token: str | None = None,
        start_after: str | None = None,
    ) -> ListObjectsV2Output: ...
    def create_bucket(self, *, bucket: str) -> CreateBucketOutput: ...
    def get_object_attributes(
        self,
        *,
        bucket: str,
        key: str,
        object_attributes: Iterable[
            Literal["ETag", "Checksum", "ObjectParts", "StorageClass", "ObjectSize"]
        ],
        expected_bucket_owner: str | None = None,
        max_parts: int | None = None,
        part_number_marker: int | None = None,
    ) -> GetObjectAttributesOutput: ...
    def get_object_tagging(
        self, *, bucket: str, key: str, expected_bucket_owner: str | None = None
    ) -> GetObjectTaggingOutput: ...
    def put_object_tagging(
        self,
        *,
        bucket: str,
        key: str,
        tags: Iterable[Tag],
        expected_bucket_owner: str | None = None,
    ) -> PutObjectTaggingOutput: ...
    def delete_object_tagging(
        self, *, bucket: str, key: str, expected_bucket_owner: str | None = None
    ) -> DeleteObjectTaggingOutput: ...
    def put_bucket_cors(
        self,
        *,
        bucket: str,
        cors_rules: Iterable[CorsRule],
        expected_bucket_owner: str | None = None,
    ) -> PutBucketCorsOutput: ...
    def get_bucket_cors(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> GetBucketCorsOutput: ...
    def delete_bucket_cors(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> DeleteBucketCorsOutput: ...
    def put_bucket_lifecycle_configuration(
        self,
        *,
        bucket: str,
        lifecycle_rules: Iterable[LifecycleRule],
        expected_bucket_owner: str | None = None,
    ) -> PutBucketLifecycleConfigurationOutput: ...
    def get_bucket_lifecycle_configuration(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> GetBucketLifecycleConfigurationOutput: ...
    def delete_bucket_lifecycle(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> DeleteBucketLifecycleOutput: ...
    def put_bucket_policy(
        self, *, bucket: str, document: str, expected_bucket_owner: str | None = None
    ) -> PutBucketPolicyOutput: ...
    def get_bucket_policy(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> GetBucketPolicyOutput: ...
    def delete_bucket_policy(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> DeleteBucketPolicyOutput: ...
    def get_bucket_policy_status(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> GetBucketPolicyStatusOutput: ...
    def put_public_access_block(
        self,
        *,
        bucket: str,
        configuration: PublicAccessBlockConfiguration,
        expected_bucket_owner: str | None = None,
    ) -> PutPublicAccessBlockOutput: ...
    def get_public_access_block(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> GetPublicAccessBlockOutput: ...
    def delete_public_access_block(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> DeletePublicAccessBlockOutput: ...
    def put_bucket_ownership_controls(
        self,
        *,
        bucket: str,
        object_ownership: Literal["BucketOwnerEnforced"],
        expected_bucket_owner: str | None = None,
    ) -> PutBucketOwnershipControlsOutput: ...
    def get_bucket_ownership_controls(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> GetBucketOwnershipControlsOutput: ...
    def get_bucket_acl(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> GetBucketAclOutput: ...
    def get_object_acl(
        self, *, bucket: str, key: str, expected_bucket_owner: str | None = None
    ) -> GetObjectAclOutput: ...
    def head_bucket(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> HeadBucketOutput: ...
    def get_bucket_location(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> GetBucketLocationOutput: ...
    def delete_bucket(
        self, *, bucket: str, expected_bucket_owner: str | None = None
    ) -> DeleteBucketOutput: ...
    def put_object(
        self,
        *,
        bucket: str,
        key: str,
        body: bytes | BinaryReader | Iterable[bytes | bytearray | memoryview],
        expected_bucket_owner: str | None = None,
        content_length: int | None = None,
        content_type: str | None = None,
        content_encoding: str | None = None,
        content_disposition: str | None = None,
        content_language: str | None = None,
        cache_control: str | None = None,
        expires: datetime | None = None,
        metadata: dict[str, str] | None = None,
        tags: Iterable[Tag] | None = None,
        checksum_algorithm: Literal["CRC32", "CRC32C", "CRC64NVME", "SHA1", "SHA256"]
        | None = None,
        content_md5: str | None = None,
        checksum_crc32: str | None = None,
        checksum_crc32c: str | None = None,
        checksum_sha1: str | None = None,
        checksum_sha256: str | None = None,
        checksum_crc64_nvme: str | None = None,
        if_match: str | None = None,
        if_none_match: str | None = None,
    ) -> PutObjectOutput: ...
    def get_object(
        self,
        *,
        bucket: str,
        key: str,
        expected_bucket_owner: str | None = None,
        checksum_mode: Literal["ENABLED"] | None = None,
        range: str | None = None,
        part_number: int | None = None,
        if_match: str | None = None,
        if_none_match: str | None = None,
        if_modified_since: datetime | None = None,
        if_unmodified_since: datetime | None = None,
        response_content_type: str | None = None,
        response_content_encoding: str | None = None,
        response_content_disposition: str | None = None,
        response_content_language: str | None = None,
        response_cache_control: str | None = None,
        response_expires: datetime | None = None,
    ) -> GetObjectOutput: ...
    def head_object(
        self,
        *,
        bucket: str,
        key: str,
        expected_bucket_owner: str | None = None,
        checksum_mode: Literal["ENABLED"] | None = None,
        range: str | None = None,
        part_number: int | None = None,
        if_match: str | None = None,
        if_none_match: str | None = None,
        if_modified_since: datetime | None = None,
        if_unmodified_since: datetime | None = None,
        response_content_type: str | None = None,
        response_content_encoding: str | None = None,
        response_content_disposition: str | None = None,
        response_content_language: str | None = None,
        response_cache_control: str | None = None,
        response_expires: datetime | None = None,
    ) -> HeadObjectOutput: ...
    def delete_objects(
        self,
        *,
        bucket: str,
        objects: Iterable[ObjectIdentifier],
        expected_bucket_owner: str | None = None,
        quiet: bool | None = None,
    ) -> DeleteObjectsOutput: ...
    def delete_object(
        self,
        *,
        bucket: str,
        key: str,
        expected_bucket_owner: str | None = None,
        if_match: str | None = None,
    ) -> DeleteObjectOutput: ...
    def close(self) -> None: ...
    def __enter__(self) -> Self: ...
    def __exit__(
        self,
        _exception_type: type[BaseException] | None,
        exception: BaseException | None,
        _traceback: TracebackType | None,
    ) -> Literal[False]: ...

class BinaryReader(Protocol):
    def read(self, size: int = -1, /) -> bytes: ...

@final
class Tag:
    def __new__(cls, *, key: str, value: str) -> Self: ...
    @property
    def key(self) -> str: ...
    @property
    def value(self) -> str: ...

@final
class CreateBucketOutput: ...

@final
class DeleteBucketOutput: ...

@final
class DeleteObjectOutput: ...

@final
class HeadBucketOutput:
    @property
    def bucket_region(self) -> str: ...

@final
class GetBucketLocationOutput:
    @property
    def location_constraint(self) -> str | None: ...

@final
class ObjectBody:
    def read_chunk(self, *, max_bytes: int) -> bytes | None: ...
    def read_all(self, *, max_bytes: int) -> bytes: ...
    def close(self) -> None: ...
    def __enter__(self) -> Self: ...
    def __exit__(
        self,
        _exception_type: type[BaseException] | None,
        exception: BaseException | None,
        _traceback: TracebackType | None,
    ) -> Literal[False]: ...

@final
class PutObjectOutput:
    @property
    def etag(self) -> str: ...
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...

@final
class GetObjectOutput:
    @property
    def etag(self) -> str: ...
    @property
    def body(self) -> ObjectBody: ...
    @property
    def content_length(self) -> int: ...
    @property
    def last_modified(self) -> datetime: ...
    @property
    def metadata(self) -> Mapping[str, str]: ...
    @property
    def tag_count(self) -> int: ...
    @property
    def content_type(self) -> str | None: ...
    @property
    def content_encoding(self) -> str | None: ...
    @property
    def content_disposition(self) -> str | None: ...
    @property
    def content_language(self) -> str | None: ...
    @property
    def cache_control(self) -> str | None: ...
    @property
    def content_range(self) -> str | None: ...
    @property
    def expires(self) -> datetime | None: ...
    @property
    def parts_count(self) -> int | None: ...
    @property
    def checksum_type(self) -> Literal["FULL_OBJECT", "COMPOSITE"] | None: ...
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...

@final
class HeadObjectOutput:
    @property
    def etag(self) -> str: ...
    @property
    def content_length(self) -> int: ...
    @property
    def last_modified(self) -> datetime: ...
    @property
    def metadata(self) -> Mapping[str, str]: ...
    @property
    def tag_count(self) -> int: ...
    @property
    def content_type(self) -> str | None: ...
    @property
    def content_encoding(self) -> str | None: ...
    @property
    def content_disposition(self) -> str | None: ...
    @property
    def content_language(self) -> str | None: ...
    @property
    def cache_control(self) -> str | None: ...
    @property
    def content_range(self) -> str | None: ...
    @property
    def expires(self) -> datetime | None: ...
    @property
    def parts_count(self) -> int | None: ...
    @property
    def checksum_type(self) -> Literal["FULL_OBJECT", "COMPOSITE"] | None: ...
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...

@final
class OwnerInfo:
    @property
    def account_id(self) -> str: ...
    @property
    def canonical_id(self) -> str: ...

@final
class PrincipalInfo:
    @property
    def principal_id(self) -> str: ...
    @property
    def name(self) -> str: ...
    @property
    def is_enabled(self) -> bool: ...
    @property
    def created_at(self) -> datetime: ...

@final
class AccessKeyInfo:
    @property
    def access_key_id(self) -> str: ...
    @property
    def principal_id(self) -> str: ...
    @property
    def created_at(self) -> datetime: ...

@final
class CreateAccessKeyOutput:
    @property
    def access_key(self) -> AccessKeyInfo: ...
    @property
    def secret_access_key(self) -> str: ...

@final
class ListPrincipalsOutput:
    @property
    def principals(self) -> tuple[PrincipalInfo, ...]: ...
    @property
    def next_continuation_token(self) -> str | None: ...

@final
class ListAccessKeysOutput:
    @property
    def access_keys(self) -> tuple[AccessKeyInfo, ...]: ...
    @property
    def next_continuation_token(self) -> str | None: ...

@final
class GetPrincipalPolicyOutput:
    @property
    def document(self) -> str: ...

@final
class PutPrincipalPolicyOutput: ...

@final
class DeletePrincipalPolicyOutput: ...

@final
class DeleteAccessKeyOutput: ...

@final
class DeletePrincipalOutput: ...

@final
class GetObjectTaggingOutput:
    def __init__(self) -> None: ...
    @property
    def tags(self) -> tuple[Tag, ...]: ...

@final
class PutObjectTaggingOutput:
    def __init__(self) -> None: ...

@final
class DeleteObjectTaggingOutput:
    def __init__(self) -> None: ...

@final
class ObjectRef:
    def __new__(cls, *, bucket: str, key: str) -> Self: ...
    @property
    def bucket(self) -> str: ...
    @property
    def key(self) -> str: ...

@final
class CompletedPart:
    def __new__(
        cls,
        *,
        part_number: int,
        etag: str,
        checksum_crc32: str | None = None,
        checksum_crc32c: str | None = None,
        checksum_sha1: str | None = None,
        checksum_sha256: str | None = None,
        checksum_crc64_nvme: str | None = None,
    ) -> Self: ...
    @property
    def part_number(self) -> int: ...
    @property
    def etag(self) -> str: ...
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...

@final
class CopyObjectOutput:
    @property
    def etag(self) -> str: ...
    @property
    def last_modified(self) -> datetime: ...
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...

@final
class CreateMultipartUploadOutput:
    @property
    def upload_id(self) -> str: ...
    @property
    def checksum_algorithm(
        self,
    ) -> Literal["CRC32", "CRC32C", "CRC64NVME", "SHA1", "SHA256"]: ...
    @property
    def checksum_type(self) -> Literal["FULL_OBJECT", "COMPOSITE"]: ...

@final
class UploadPartOutput:
    @property
    def etag(self) -> str: ...
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...

@final
class UploadPartCopyOutput:
    @property
    def etag(self) -> str: ...
    @property
    def last_modified(self) -> datetime: ...
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...

@final
class CompleteMultipartUploadOutput:
    @property
    def etag(self) -> str: ...
    @property
    def checksum_type(self) -> Literal["FULL_OBJECT", "COMPOSITE"]: ...
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...

@final
class AbortMultipartUploadOutput:
    pass

@final
class PartSummary:
    @property
    def part_number(self) -> int: ...
    @property
    def size(self) -> int: ...
    @property
    def etag(self) -> str: ...
    @property
    def last_modified(self) -> datetime: ...
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...

@final
class MultipartUploadSummary:
    @property
    def key(self) -> str: ...
    @property
    def upload_id(self) -> str: ...
    @property
    def initiated(self) -> datetime: ...
    @property
    def checksum_algorithm(
        self,
    ) -> Literal["CRC32", "CRC32C", "CRC64NVME", "SHA1", "SHA256"]: ...
    @property
    def checksum_type(self) -> Literal["FULL_OBJECT", "COMPOSITE"]: ...

@final
class ListPartsOutput:
    @property
    def parts(self) -> tuple[PartSummary, ...]: ...
    @property
    def is_truncated(self) -> bool: ...
    @property
    def next_part_number_marker(self) -> int | None: ...
    @property
    def checksum_algorithm(
        self,
    ) -> Literal["CRC32", "CRC32C", "CRC64NVME", "SHA1", "SHA256"]: ...
    @property
    def checksum_type(self) -> Literal["FULL_OBJECT", "COMPOSITE"]: ...

@final
class ListMultipartUploadsOutput:
    @property
    def uploads(self) -> tuple[MultipartUploadSummary, ...]: ...
    @property
    def common_prefixes(self) -> tuple[CommonPrefix, ...]: ...
    @property
    def is_truncated(self) -> bool: ...
    @property
    def next_key_marker(self) -> str | None: ...
    @property
    def next_upload_id_marker(self) -> str | None: ...

@final
class Checksum:
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...
    @property
    def checksum_type(self) -> Literal["FULL_OBJECT", "COMPOSITE"] | None: ...

@final
class ObjectPart:
    @property
    def checksum_crc32(self) -> str | None: ...
    @property
    def checksum_crc32c(self) -> str | None: ...
    @property
    def checksum_sha1(self) -> str | None: ...
    @property
    def checksum_sha256(self) -> str | None: ...
    @property
    def checksum_crc64_nvme(self) -> str | None: ...
    @property
    def part_number(self) -> int: ...
    @property
    def size(self) -> int: ...

@final
class ObjectParts:
    @property
    def total_parts_count(self) -> int: ...
    @property
    def max_parts(self) -> int: ...
    @property
    def part_number_marker(self) -> int: ...
    @property
    def parts(self) -> tuple[ObjectPart, ...]: ...
    @property
    def is_truncated(self) -> bool: ...
    @property
    def next_part_number_marker(self) -> int | None: ...

@final
class GetObjectAttributesOutput:
    @property
    def object_size(self) -> int | None: ...
    @property
    def etag(self) -> str | None: ...
    @property
    def checksum(self) -> Checksum | None: ...
    @property
    def storage_class(self) -> str | None: ...
    @property
    def object_parts(self) -> ObjectParts | None: ...
    @property
    def last_modified(self) -> datetime: ...
