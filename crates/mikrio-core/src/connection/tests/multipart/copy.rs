use super::*;
use crate::OperationError;
use crate::integrity::{PutIntegrity, PutIntegrityInput};
use crate::multipart::{
    CompleteRequest, CompletionParts, CopyPartRequest, ListPartsRequest, PartNumber, PartPage,
    PartPageSize, SelectedPart, UploadRequest,
};

use crate::object::{ObjectRef, ObjectTags, ReadConditions, WriteConditions};

mod authorization;
mod outcomes;
mod rejection;

pub(super) fn input(upload_id: UploadId, number: u32) -> CopyPartRequest {
    CopyPartRequest {
        bucket: request().bucket,
        key: request().key,
        upload_id,
        part_number: PartNumber::new(number).unwrap(),
        source: ObjectRef {
            bucket: request().bucket,
            key: ObjectKey::parse("source").unwrap(),
        },
        expected_owner: None,
        expected_source_owner: None,
        source_conditions: ReadConditions::default(),
        source_range: None,
    }
}

fn upload(upload_id: UploadId) -> UploadRequest {
    UploadRequest {
        bucket: request().bucket,
        key: request().key,
        upload_id,
        expected_owner: None,
    }
}

async fn page(connection: &Connection, upload_id: UploadId) -> Result<PartPage, OperationError> {
    connection
        .list_parts(ListPartsRequest {
            upload: upload(upload_id),
            part_number_marker: 0,
            max_parts: PartPageSize::default(),
        })
        .await
}

async fn complete(connection: &Connection, upload_id: UploadId) -> TestResult {
    let selected = page(connection, upload_id.clone()).await?;

    connection
        .complete_multipart_upload(CompleteRequest {
            upload: upload(upload_id),
            parts: CompletionParts::new(selected.parts.into_iter().map(|part| SelectedPart {
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

#[test]
fn copied_parts_use_upload_negotiation_and_accepted_time_without_inheriting_source_metadata()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.create_bucket(request().bucket).await?;
        let mut source = super::super::objects::put("source");
        source.metadata = UserMetadata::new([("origin".into(), "source".into())])?;
        source.tags = Some(ObjectTags::new([("team".into(), "source".into())])?);

        source.integrity = PutIntegrity::new(PutIntegrityInput {
            checksum_algorithm: Some(ChecksumAlgorithm::Sha1),
            ..PutIntegrityInput::default()
        })?;

        connection.put_object(source, b"abc".as_slice()).await?;

        for (algorithm, mode, expected) in [
            (
                ChecksumAlgorithm::Crc32,
                ChecksumType::FullObject,
                "NSRBwg==",
            ),
            (
                ChecksumAlgorithm::Crc32,
                ChecksumType::Composite,
                "NSRBwg==",
            ),
            (
                ChecksumAlgorithm::Crc32c,
                ChecksumType::FullObject,
                "Nks/tw==",
            ),
            (
                ChecksumAlgorithm::Crc32c,
                ChecksumType::Composite,
                "Nks/tw==",
            ),
            (
                ChecksumAlgorithm::Crc64Nvme,
                ChecksumType::FullObject,
                "BeXKuz/B+us=",
            ),
            (
                ChecksumAlgorithm::Sha1,
                ChecksumType::Composite,
                "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=",
            ),
            (
                ChecksumAlgorithm::Sha256,
                ChecksumType::Composite,
                "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=",
            ),
        ] {
            let mut initiation = request();
            initiation.metadata = UserMetadata::new([("origin".into(), "initiation".into())])?;
            initiation.tags = Some(ObjectTags::new([("team".into(), "initiation".into())])?);
            initiation.checksum = MultipartChecksum::new(algorithm, mode)?;
            let created = connection.create_multipart_upload(initiation).await?;

            connection
                .upload_part(
                    super::parts::part(created.upload_id.clone(), 1),
                    b"before".as_slice(),
                )
                .await?;

            let copied = connection
                .upload_part_copy(input(created.upload_id.clone(), 1))
                .await?;

            assert_eq!(copied.facts.etag(), "\"900150983cd24fb0d6963f7d28e17f72\"");
            assert_eq!(copied.facts.checksum().encode(), expected);
            assert_eq!(copied.facts.checksum().algorithm(), algorithm);
            let selected = page(&connection, created.upload_id.clone()).await?;
            assert_eq!(selected.parts.len(), 1);
            assert_eq!(selected.parts[0].last_modified, copied.last_modified);
            assert_eq!(selected.parts[0].checksum, copied.facts.checksum());
            complete(&connection, created.upload_id).await?;

            let (selected, mut body) = connection
                .get_object(super::super::objects::read(request().key.as_str()))
                .await?;

            assert_eq!(body.read_all(3).await?, b"abc");
            body.close().await?;

            assert_eq!(
                selected.metadata,
                UserMetadata::new([("origin".into(), "initiation".into())])?
            );

            assert_eq!(
                selected.tags,
                ObjectTags::new([("team".into(), "initiation".into())])?
            );
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn copied_and_uploaded_parts_complete_after_source_deletion_with_independent_bytes() -> TestResult {
    use sha2::Digest as _;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::objects::put("source"),
                std::io::Cursor::new(vec![b'a'; 5 * 1024 * 1024]),
            )
            .await?;

        let created = connection.create_multipart_upload(request()).await?;

        connection
            .upload_part_copy(input(created.upload_id.clone(), 1))
            .await?;

        connection
            .delete_object(super::super::objects::read("source"), None)
            .await?;

        connection
            .upload_part(
                super::parts::part(created.upload_id.clone(), 7),
                b"tail".as_slice(),
            )
            .await?;

        complete(&connection, created.upload_id).await?;
        connection.close().await?;
        let reopened = Connection::open(path, None).await?;

        let (_, mut body) = reopened
            .get_object(super::super::objects::read(request().key.as_str()))
            .await?;

        let payload = body.read_all(6 * 1024 * 1024).await?;
        body.close().await?;

        let expected = crate::values::decode_hex::<32>(
            b"d15a55813872a596ca56a0d2119230fbce8dc3c0ccb36fde29fd36b6d764bb7f",
        )?;

        assert_eq!(sha2::Sha256::digest(payload).as_slice(), expected);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn same_key_part_copy_selects_the_completed_object_and_preserves_empty_bytes() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::objects::put(request().key.as_str()),
                b"".as_slice(),
            )
            .await?;

        let created = connection.create_multipart_upload(request()).await?;
        let mut copy = input(created.upload_id.clone(), 1);
        copy.source.key = request().key;
        let copied = connection.upload_part_copy(copy).await?;
        assert_eq!(copied.facts.length(), 0);
        assert_eq!(copied.facts.etag(), "\"d41d8cd98f00b204e9800998ecf8427e\"");
        complete(&connection, created.upload_id).await?;

        let selected = connection
            .head_object(super::super::objects::read(request().key.as_str()))
            .await?;

        assert_eq!(selected.length.get(), 0);
        assert_eq!(selected.etag, "\"59adb24ef3cdbe0297f05b395827453f-1\"");
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
