//! Real operations seed the feature state; literal bytes and facts verify its restore.

use super::*;
use crate::access::{
    AccessKeyInfo, AdminPageSize, BucketAccessRequest, PrincipalInfo, PrincipalName,
};
use crate::connection::Connection;
use crate::integrity::{ChecksumAlgorithm, ChecksumType, ExpectedDigests};
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};
use crate::multipart::{
    CompleteRequest, CompletionParts, CreateRequest, ListPartsRequest, ListedPart,
    MultipartChecksum, PartNumber, PartPageSize, PartRequest, SelectedPart, UploadRequest,
};
use crate::object::{
    Attribute, AttributeSelection, AttributesRequest, BucketName, ObjectKey, ObjectTags,
    ReadConditions, ReadRequest, Representation, UserMetadata, WriteConditions,
};
use crate::{PublicAccessBlock, UploadId};

const POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::attachments/*"}}"#;
const BUCKET_POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:DeleteObject","Resource":"arn:aws:s3:::attachments/protected/*"}}"#;

pub(super) struct Expected {
    pub(super) identity: StoreRecord,
    active: UploadId,
    part: ListedPart,
    principal: PrincipalInfo,
    key: AccessKeyInfo,
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()
}

fn bucket() -> BucketAccessRequest {
    BucketAccessRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        expected_owner: None,
    }
}

fn lifecycle() -> LifecycleConfiguration {
    LifecycleConfiguration::new(vec![LifecycleRule {
        id: Some("retained-rule".into()),
        is_enabled: true,
        filter: LifecycleFilter::Prefix("future/".into()),
        expiration_days: Some(7),
        abort_incomplete_multipart_days: Some(3),
    }])
    .unwrap()
}

fn public_access_block() -> PublicAccessBlock {
    PublicAccessBlock {
        block_public_acls: true,
        ignore_public_acls: false,
        block_public_policy: true,
        restrict_public_buckets: false,
    }
}

fn create(key: &str) -> CreateRequest {
    CreateRequest {
        bucket: bucket().bucket,
        key: ObjectKey::parse(key).unwrap(),
        expected_owner: None,
        representation: Representation::default(),
        metadata: UserMetadata::new([("title".into(), "café".into())]).unwrap(),
        tags: Some(ObjectTags::new([("purpose".into(), "restore".into())]).unwrap()),
        checksum: MultipartChecksum::new(ChecksumAlgorithm::Crc64Nvme, ChecksumType::FullObject)
            .unwrap(),
    }
}

fn upload(key: &str, id: &UploadId) -> UploadRequest {
    UploadRequest {
        bucket: bucket().bucket,
        key: ObjectKey::parse(key).unwrap(),
        upload_id: id.clone(),
        expected_owner: None,
    }
}

fn part(key: &str, id: &UploadId, number: u32) -> PartRequest {
    PartRequest {
        bucket: bucket().bucket,
        key: ObjectKey::parse(key).unwrap(),
        upload_id: id.clone(),
        part_number: PartNumber::new(number).unwrap(),
        expected_owner: None,
        content_length: None,
        checksum_algorithm: None,
        expected: ExpectedDigests::default(),
    }
}

fn read(key: &str) -> ReadRequest {
    ReadRequest {
        bucket: bucket().bucket,
        key: ObjectKey::parse(key).unwrap(),
        expected_owner: None,
        conditions: ReadConditions::default(),
        range: None,
        part_number: None,
        response: Representation::default(),
    }
}

async fn parts(
    connection: &Connection,
    key: &str,
    id: &UploadId,
) -> Result<Vec<ListedPart>, Box<dyn std::error::Error>> {
    let page = connection
        .list_parts(ListPartsRequest {
            upload: upload(key, id),
            part_number_marker: 0,
            max_parts: PartPageSize::default(),
        })
        .await?;

    assert_eq!(page.next_part_number_marker, None);
    Ok(page.parts)
}

async fn complete(connection: &Connection, key: &str, id: &UploadId) -> TestResult {
    let parts = parts(connection, key, id).await?;

    connection
        .complete_multipart_upload(CompleteRequest {
            upload: upload(key, id),
            parts: CompletionParts::new(parts.into_iter().map(|part| SelectedPart {
                part_number: part.part_number,
                etag: part.etag,
                checksum: Some(part.checksum),
            }))?,
            conditions: WriteConditions::default(),
            content_length: None,
            checksum_type: None,
            checksum: None,
        })
        .await?;

    Ok(())
}

pub(super) fn seed(path: &Path) -> Result<Expected, Box<dyn std::error::Error>> {
    runtime()?.block_on(async {
        let connection = Connection::open(path.to_path_buf(), Some("eu-west-1".into())).await?;
        connection.pause_maintenance();
        connection.create_bucket(bucket().bucket).await?;

        let completed = connection
            .create_multipart_upload(create("completed"))
            .await?;

        connection
            .upload_part(
                part("completed", &completed.upload_id, 2),
                std::io::Cursor::new(vec![b'a'; 5 * 1024 * 1024]),
            )
            .await?;

        connection
            .upload_part(
                part("completed", &completed.upload_id, 7),
                b"tail".as_slice(),
            )
            .await?;

        complete(&connection, "completed", &completed.upload_id).await?;

        let active = connection.create_multipart_upload(create("active")).await?;

        connection
            .upload_part(
                part("active", &active.upload_id, 3),
                b"resume me".as_slice(),
            )
            .await?;

        let mut active_parts = parts(&connection, "active", &active.upload_id).await?;
        assert_eq!(active_parts.len(), 1);
        let active_part = active_parts.remove(0);

        let retired = connection
            .create_multipart_upload(create("retired"))
            .await?;

        connection
            .abort_multipart_upload(upload("retired", &retired.upload_id))
            .await?;

        let principal = connection
            .create_principal(PrincipalName::new("restore-reader".into())?)
            .await?;

        connection
            .put_principal_policy(principal.principal_id, POLICY.into())
            .await?;

        let key = connection.create_access_key(principal.principal_id).await?;

        connection
            .put_bucket_policy(bucket(), BUCKET_POLICY.into())
            .await?;

        connection
            .put_public_access_block(bucket(), public_access_block())
            .await?;

        connection
            .put_bucket_lifecycle(bucket(), lifecycle())
            .await?;

        let expected = Expected {
            identity: connection.info().record.clone(),
            active: active.upload_id,
            part: active_part,
            principal,
            key: key.access_key,
        };

        connection.close().await?;
        Ok(expected)
    })
}

pub(super) fn verify_and_resume(path: &Path, expected: &Expected) -> TestResult {
    runtime()?.block_on(async {
        let connection = Connection::open(path.to_path_buf(), None).await?;
        assert_eq!(connection.info().record, expected.identity);

        assert_eq!(
            connection.get_bucket_lifecycle(bucket()).await?,
            Some(lifecycle())
        );

        assert_eq!(
            connection.get_bucket_policy(bucket()).await?.document,
            BUCKET_POLICY
        );

        assert_eq!(
            connection.get_public_access_block(bucket()).await?,
            public_access_block()
        );

        assert_eq!(
            connection
                .get_principal(expected.principal.principal_id)
                .await?,
            expected.principal
        );

        assert_eq!(
            connection
                .get_principal_policy(expected.principal.principal_id)
                .await?
                .document,
            POLICY
        );

        let keys = connection
            .list_access_keys(
                expected.principal.principal_id,
                AdminPageSize::default(),
                None,
            )
            .await?;

        assert_eq!(keys.access_keys, vec![expected.key.clone()]);
        assert_eq!(keys.next_continuation_token, None);

        assert_eq!(
            parts(&connection, "active", &expected.active).await?,
            vec![expected.part.clone()]
        );

        verify_completed(&connection).await?;
        complete(&connection, "active", &expected.active).await?;
        connection.close().await?;

        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                // Inspect only after all engine handles have closed.
                let pending: i64 = sql(path)?.query_row(
                    "SELECT (SELECT count(*) FROM blobs WHERE gc_pending = 1)
                     + (SELECT count(*) FROM multipart_receipts)",
                    [],
                    |row| row.get(0),
                )?;

                if pending == 0 {
                    return Ok::<_, Box<dyn std::error::Error>>(());
                }

                let worker = Connection::open(path.to_path_buf(), None).await?;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                worker.close().await?;
            }
        })
        .await??;

        let connection = Connection::open(path.to_path_buf(), None).await?;
        let (selected, mut body) = connection.get_object(read("active")).await?;
        assert_eq!(body.read_all(100).await?, b"resume me");
        assert_eq!(selected.metadata, create("active").metadata);
        assert_eq!(selected.tags, create("active").tags.unwrap());
        body.close().await?;
        verify_completed(&connection).await?;
        connection.close().await?;
        Ok(())
    })
}

async fn verify_completed(connection: &Connection) -> TestResult {
    use sha2::Digest as _;

    let (selected, mut body) = connection.get_object(read("completed")).await?;
    assert_eq!(selected.etag, "\"30dcfd3901d1c613b7fb532281748544-2\"");
    assert_eq!(selected.metadata, create("completed").metadata);
    assert_eq!(selected.tags, create("completed").tags.unwrap());
    let bytes = body.read_all(6 * 1024 * 1024).await?;

    let digest = crate::values::decode_hex::<32>(
        b"d15a55813872a596ca56a0d2119230fbce8dc3c0ccb36fde29fd36b6d764bb7f",
    )?;

    assert_eq!(sha2::Sha256::digest(&bytes).as_slice(), digest);
    body.close().await?;

    let attributes = connection
        .get_object_attributes(AttributesRequest {
            bucket: bucket().bucket,
            key: ObjectKey::parse("completed")?,
            expected_owner: None,
            attributes: AttributeSelection::new([Attribute::ObjectParts, Attribute::ObjectSize])?,
            max_parts: PartPageSize::default(),
            part_number_marker: 0,
        })
        .await?;

    assert_eq!(attributes.object_size.unwrap().get(), 5 * 1024 * 1024 + 4);
    let parts = attributes.object_parts.unwrap();
    assert_eq!(parts.total_parts_count, 2);
    assert_eq!(parts.next_part_number_marker, None);
    assert!(!parts.entries_available);
    assert_eq!(parts.parts, []);

    for (number, offset, length) in [(1, 0, 5 * 1024 * 1024), (2, 5 * 1024 * 1024, 4)] {
        let mut input = read("completed");
        input.part_number = Some(crate::multipart::PartNumber::new(number)?);
        let selected = connection.head_object(input).await?;
        assert_eq!(selected.content_offset(), offset);
        assert_eq!(selected.content_length(), length);
        assert_eq!(selected.parts_count(), Some(2));
    }

    Ok(())
}
