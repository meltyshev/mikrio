use super::*;
use crate::integrity::{ChecksumAlgorithm, ChecksumType, PutIntegrity, PutIntegrityInput};
use crate::object::{
    BucketName, CopyDirective, CopyRequest, ObjectKey, ObjectRef, ObjectTags, ReadConditions,
    ReadConditionsInput, Representation, RepresentationInput, UserMetadata, WriteConditions,
};

use crate::{MutationOutcome, OperationFailure};

mod authorization;
mod multipart;
mod outcomes;
mod shards;

fn request() -> CopyRequest {
    CopyRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse("destination").unwrap(),
        source: ObjectRef {
            bucket: BucketName::parse("attachments").unwrap(),
            key: ObjectKey::parse("source").unwrap(),
        },
        expected_owner: None,
        expected_source_owner: None,
        source_conditions: ReadConditions::default(),
        conditions: WriteConditions::default(),
        metadata_directive: CopyDirective::Copy,
        representation: Representation::default(),
        metadata: UserMetadata::default(),
        tagging_directive: CopyDirective::Copy,
        tags: None,
        checksum_algorithm: None,
    }
}

async fn bytes(connection: &Connection, key: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let (_, mut body) = connection.get_object(objects::read(key)).await?;
    let bytes = body.read_all(1024).await?;
    body.close().await?;
    Ok(bytes)
}

#[test]
fn copy_directives_are_independent_and_publish_new_full_object_facts() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;
        let mut source = objects::put("source");

        source.representation = Representation::new(RepresentationInput {
            content_type: Some("text/source".into()),
            cache_control: Some("max-age=12".into()),
            ..RepresentationInput::default()
        })?;

        source.metadata = UserMetadata::new([("origin".into(), "source".into())])?;
        source.tags = Some(ObjectTags::new([("team".into(), "source".into())])?);

        source.integrity = PutIntegrity::new(PutIntegrityInput {
            checksum_algorithm: Some(ChecksumAlgorithm::Sha256),
            ..PutIntegrityInput::default()
        })?;

        connection.put_object(source, b"abc".as_slice()).await?;
        let original = connection.head_object(objects::read("source")).await?;

        for (index, metadata, tags) in [
            (0, CopyDirective::Copy, CopyDirective::Copy),
            (1, CopyDirective::Copy, CopyDirective::Replace),
            (2, CopyDirective::Replace, CopyDirective::Copy),
            (3, CopyDirective::Replace, CopyDirective::Replace),
        ] {
            let mut input = request();
            input.key = ObjectKey::parse(&format!("destination-{index}"))?;
            input.metadata_directive = metadata;
            input.tagging_directive = tags;
            input.metadata = UserMetadata::new([("origin".into(), "replacement".into())])?;

            input.representation = Representation::new(RepresentationInput {
                content_type: Some("text/replacement".into()),
                ..RepresentationInput::default()
            })?;

            if tags == CopyDirective::Replace {
                input.tags = Some(ObjectTags::new([("team".into(), "replacement".into())])?);
            }

            let result = connection.copy_object(input.clone()).await?;
            assert_eq!(result.facts.etag(), "\"900150983cd24fb0d6963f7d28e17f72\"");

            assert_eq!(
                result.facts.checksum().encode(),
                "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="
            );

            let copied = connection
                .head_object(objects::read(input.key.as_str()))
                .await?;

            assert_ne!(copied.blob_id, original.blob_id);
            assert_ne!(copied.generation, original.generation);
            assert_eq!(copied.last_modified, result.last_modified);
            assert_eq!(copied.checksum_type, Some(ChecksumType::FullObject));
            assert_eq!(bytes(&connection, input.key.as_str()).await?, b"abc");

            assert_eq!(
                copied.representation,
                if metadata == CopyDirective::Copy {
                    original.representation.clone()
                } else {
                    input.representation
                }
            );

            assert_eq!(
                copied.metadata,
                if metadata == CopyDirective::Copy {
                    original.metadata.clone()
                } else {
                    input.metadata
                }
            );

            assert_eq!(
                copied.tags,
                if tags == CopyDirective::Copy {
                    original.tags.clone()
                } else {
                    input.tags.unwrap()
                }
            );
        }

        let mut replaced = request();
        replaced.tagging_directive = CopyDirective::Replace;
        replaced.checksum_algorithm = Some(ChecksumAlgorithm::Crc32);
        let result = connection.copy_object(replaced).await?;
        assert_eq!(result.facts.checksum().encode(), "NSRBwg==");

        assert_eq!(
            connection
                .head_object(objects::read("destination"))
                .await?
                .tags,
            ObjectTags::default()
        );

        let mut invalid = request();
        invalid.tags = Some(ObjectTags::default());

        assert_eq!(
            connection.copy_object(invalid).await.unwrap_err().failure(),
            OperationFailure::InvalidArgument
        );

        let mut self_copy = request();
        self_copy.key = self_copy.source.key.clone();

        assert_eq!(
            connection
                .copy_object(self_copy.clone())
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidRequest
        );

        self_copy.metadata_directive = CopyDirective::Replace;
        connection.copy_object(self_copy).await?;
        assert_eq!(bytes(&connection, "source").await?, b"abc");
        connection.close().await?;

        let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        assert_eq!(
            database.query_row(
                "SELECT count(*) FROM objects WHERE last_modified_at_ms <> lifecycle_anchor_at_ms",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );

        assert_eq!(
            database.query_row(
                "SELECT count(*) FROM blobs WHERE write_kind = 'copy_object' AND part_count IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );

        database.close().map_err(|(_, error)| error)?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn copy_conditions_owner_assertions_and_corrupt_bytes_preserve_the_previous_destination()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    let source = runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(objects::put("source"), b"abc".as_slice())
            .await?;

        connection
            .put_object(objects::put("destination"), b"before".as_slice())
            .await?;

        let source = connection.head_object(objects::read("source")).await?;
        let owner = connection.get_owner_info().await?;

        let wrong = if owner.account_id() == "000000000000" {
            "111111111111"
        } else {
            "000000000000"
        };

        for (index, expected) in [
            OperationFailure::PreconditionFailed,
            OperationFailure::PreconditionFailed,
            OperationFailure::AccessDenied,
            OperationFailure::AccessDenied,
        ]
        .into_iter()
        .enumerate()
        {
            let mut input = request();

            match index {
                0 => {
                    input.source_conditions = ReadConditions::new(ReadConditionsInput {
                        if_none_match: Some(source.etag.clone()),
                        ..ReadConditionsInput::default()
                    })?;
                }
                1 => input.conditions = WriteConditions::new(None, Some("*".into()))?,
                2 => input.expected_source_owner = Some(wrong.into()),
                3 => input.expected_owner = Some(wrong.into()),
                _ => return Err("unknown copy condition fixture".into()),
            }

            let error = connection.copy_object(input).await.unwrap_err();
            assert_eq!(error.failure(), expected);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
            assert_eq!(bytes(&connection, "destination").await?, b"before");
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(source)
    })?;

    std::fs::write(
        path.join("objects")
            .join(format!("{:02x}", source.blob_id.shard()))
            .join(source.blob_id.to_string()),
        b"def",
    )?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        let error = connection.copy_object(request()).await.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        assert_eq!(bytes(&connection, "destination").await?, b"before");
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn oversized_or_corrupt_source_facts_cannot_publish_copy() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    let source = runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(objects::put("source"), b"abc".as_slice())
            .await?;

        connection
            .put_object(objects::put("destination"), b"before".as_slice())
            .await?;

        let source = connection.head_object(objects::read("source")).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(source)
    })?;

    for (length, failure) in [
        (5 * 1024 * 1024 * 1024 + 1, OperationFailure::EntityTooLarge),
        (3_i64, OperationFailure::StoreCorrupt),
    ] {
        // Offline metadata fixtures exercise validation, not a large real copy.
        let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        database.execute(
            "UPDATE blobs SET byte_length=?2, checksums_json=?3 WHERE blob_id=?1",
            rusqlite::params![
                source.blob_id.as_bytes().as_slice(),
                length,
                r#"{"CRC64NVME":"AAAAAAAAAAA="}"#,
            ],
        )?;

        database.close().map_err(|(_, error)| error)?;

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            let error = connection.copy_object(request()).await.unwrap_err();
            assert_eq!(error.failure(), failure);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
            assert_eq!(bytes(&connection, "destination").await?, b"before");
            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        let query = if failure == OperationFailure::EntityTooLarge {
            "SELECT count(*) FROM blobs WHERE write_kind='copy_object'"
        } else {
            "SELECT count(*) FROM blobs WHERE write_kind='copy_object' AND state='ready'"
        };

        assert_eq!(
            database.query_row(query, [], |row| row.get::<_, i64>(0))?,
            0
        );

        database.close().map_err(|(_, error)| error)?;
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn opposite_direction_copies_progress_on_one_native_thread_with_whole_selected_facts() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.create_bucket(request().bucket).await?;
        let originals = [vec![b'a'; 64 * 1024 + 17], vec![b'b'; 64 * 1024 + 19]];

        for (key, payload, origin) in [
            ("source", &originals[0], "a"),
            ("destination", &originals[1], "b"),
        ] {
            let mut input = objects::put(key);
            input.metadata = UserMetadata::new([("origin".into(), origin.into())])?;
            input.tags = Some(ObjectTags::new([("origin".into(), origin.into())])?);

            connection
                .put_object(input, std::io::Cursor::new(payload.clone()))
                .await?;
        }

        let mut reverse = request();
        std::mem::swap(&mut reverse.key, &mut reverse.source.key);
        let forward_owner = connection.clone();
        let reverse_owner = connection.clone();
        let forward = tokio::spawn(async move { forward_owner.copy_object(request()).await });
        let reverse = tokio::spawn(async move { reverse_owner.copy_object(reverse).await });

        let results = tokio::time::timeout(Duration::from_secs(10), async {
            Ok::<_, Box<dyn std::error::Error>>([forward.await??, reverse.await??])
        })
        .await??;

        let mut blobs = Vec::new();

        for (key, copied) in ["destination", "source"].into_iter().zip(results) {
            let (selected, mut body) = connection.get_object(objects::read(key)).await?;
            let payload = body.read_all(128 * 1024).await?;
            body.close().await?;

            let origin = if payload == originals[0] {
                "a"
            } else {
                assert_eq!(payload, originals[1]);
                "b"
            };

            assert_eq!(selected.etag, copied.facts.etag());
            assert_eq!(selected.last_modified, copied.last_modified);

            assert_eq!(
                selected.metadata,
                UserMetadata::new([("origin".into(), origin.into())])?
            );

            assert_eq!(
                selected.tags,
                ObjectTags::new([("origin".into(), origin.into())])?
            );

            blobs.push(selected.blob_id);
        }

        assert_ne!(blobs[0], blobs[1]);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
