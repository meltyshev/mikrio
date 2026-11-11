//! Force the entropy result while retaining the real guard, byte and SQL paths.

use super::*;
use crate::BlobId;
use crate::multipart::{
    CompleteRequest, CompletionParts, CopyPartRequest, CreateRequest, ListPartsRequest,
    MultipartChecksum, PartNumber, PartPageSize, SelectedPart, UploadRequest,
};

async fn output(
    connection: &Connection,
    source: &str,
    shard: u8,
    suffix: u8,
) -> Result<BlobId, Box<dyn std::error::Error>> {
    let source = connection.head_object(objects::read(source)).await?;
    let mut bytes = [0; 16];
    bytes[0] = shard;
    bytes[15] = suffix;
    let blob_id = BlobId::from_bytes(bytes);

    assert!(
        connection
            .owner
            .state
            .context
            .test_hooks
            .copy_outputs
            .lock()
            .unwrap()
            .insert(*source.blob_id.as_bytes(), blob_id)
            .is_none()
    );

    Ok(blob_id)
}

async fn sources(connection: &Connection) -> TestResult {
    connection.create_bucket(request().bucket).await?;

    connection
        .put_object(objects::put("seed"), b"abc".as_slice())
        .await?;

    for (key, shard) in [("low", 0x20), ("high", 0xe0)] {
        let expected = output(connection, "seed", shard, 1).await?;
        let mut input = request();
        input.source.key = ObjectKey::parse("seed")?;
        input.key = ObjectKey::parse(key)?;
        connection.copy_object(input).await?;

        assert_eq!(
            connection.head_object(objects::read(key)).await?.blob_id,
            expected
        );
    }

    Ok(())
}

#[test]
fn forced_same_and_opposite_shard_copies_progress_with_one_native_thread() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let forward = Connection::open(path.clone(), None).await?;
        let reverse = Connection::open(path.clone(), None).await?;
        sources(&forward).await?;

        for (suffix, forward_shard, reverse_shard) in [(2, 0x20, 0xe0), (3, 0xe0, 0x20)] {
            let expected = [
                output(&forward, "low", forward_shard, suffix).await?,
                output(&reverse, "high", reverse_shard, suffix).await?,
            ];

            let mut first = request();
            first.source.key = ObjectKey::parse("low")?;
            first.key = ObjectKey::parse("forward")?;
            let mut second = request();
            second.source.key = ObjectKey::parse("high")?;
            second.key = ObjectKey::parse("reverse")?;
            let forward_owner = forward.clone();
            let reverse_owner = reverse.clone();
            let first = tokio::spawn(async move { forward_owner.copy_object(first).await });
            let second = tokio::spawn(async move { reverse_owner.copy_object(second).await });

            let results = tokio::time::timeout(Duration::from_secs(10), async {
                Ok::<_, Box<dyn std::error::Error>>([first.await??, second.await??])
            })
            .await??;

            for ((key, blob_id), copied) in [("forward", expected[0]), ("reverse", expected[1])]
                .into_iter()
                .zip(results)
            {
                let selected = forward.head_object(objects::read(key)).await?;
                assert_eq!(selected.blob_id, blob_id);
                assert_eq!(selected.etag, "\"900150983cd24fb0d6963f7d28e17f72\"");
                assert_eq!(selected.etag, copied.facts.etag());
                assert_eq!(bytes(&forward, key).await?, b"abc");
            }
        }

        forward.close().await?;
        reverse.close().await?;
        let reopened = Connection::open(path, None).await?;
        assert_eq!(bytes(&reopened, "forward").await?, b"abc");
        assert_eq!(bytes(&reopened, "reverse").await?, b"abc");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn forced_same_and_cross_shard_part_copies_publish_independent_parts() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        sources(&connection).await?;
        connection.close().await?;

        for (source, shard) in [("low", 0x20), ("low", 0xe0), ("high", 0xe0), ("high", 0x20)] {
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();
            let key = ObjectKey::parse(&format!("part-{source}-{shard}"))?;

            let created = connection
                .create_multipart_upload(CreateRequest {
                    bucket: request().bucket,
                    key: key.clone(),
                    expected_owner: None,
                    representation: Representation::default(),
                    metadata: UserMetadata::default(),
                    tags: None,
                    checksum: MultipartChecksum::new(
                        ChecksumAlgorithm::Crc64Nvme,
                        ChecksumType::FullObject,
                    )?,
                })
                .await?;

            let expected = output(
                &connection,
                source,
                shard,
                if source == "low" { 4 } else { 5 },
            )
            .await?;

            let copied = connection
                .upload_part_copy(CopyPartRequest {
                    bucket: request().bucket,
                    key: key.clone(),
                    upload_id: created.upload_id.clone(),
                    part_number: PartNumber::new(7)?,
                    source: ObjectRef {
                        bucket: request().bucket,
                        key: ObjectKey::parse(source)?,
                    },
                    expected_owner: None,
                    expected_source_owner: None,
                    source_conditions: ReadConditions::default(),
                    source_range: None,
                })
                .await?;

            assert_eq!(copied.facts.etag(), "\"900150983cd24fb0d6963f7d28e17f72\"");
            assert_eq!(copied.facts.checksum().encode(), "BeXKuz/B+us=");

            let upload = UploadRequest {
                bucket: request().bucket,
                key: key.clone(),
                upload_id: created.upload_id,
                expected_owner: None,
            };

            let page = connection
                .list_parts(ListPartsRequest {
                    upload: upload.clone(),
                    part_number_marker: 0,
                    max_parts: PartPageSize::default(),
                })
                .await?;

            assert_eq!(page.parts.len(), 1);
            assert_eq!(page.parts[0].last_modified, copied.last_modified);
            connection.close().await?;

            let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

            let blob: Vec<u8> = database.query_row(
                "SELECT blob_id FROM multipart_parts WHERE upload_id=?1 AND part_number=7",
                [upload.upload_id.to_string()],
                |row| row.get(0),
            )?;

            assert_eq!(blob, expected.as_bytes());
            database.close().map_err(|(_, error)| error)?;
            let connection = Connection::open(path.clone(), None).await?;

            connection
                .complete_multipart_upload(CompleteRequest {
                    upload,
                    parts: CompletionParts::new(page.parts.into_iter().map(|part| SelectedPart {
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

            assert_eq!(bytes(&connection, key.as_str()).await?, b"abc");
            connection.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
