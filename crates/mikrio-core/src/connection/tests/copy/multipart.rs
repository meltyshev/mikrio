use super::*;
use crate::integrity::ExpectedDigests;
use crate::multipart::{
    CompleteRequest, CompletionParts, CreateRequest, MultipartChecksum, PartNumber, PartRequest,
    SelectedPart, UploadRequest,
};

#[test]
fn copying_completed_and_empty_multipart_sources_recomputes_full_checksums_without_layout()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        for (index, payload, etag, checksum) in [
            (
                0,
                b"abc".as_slice(),
                "\"900150983cd24fb0d6963f7d28e17f72\"",
                "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=",
            ),
            (
                1,
                b"".as_slice(),
                "\"d41d8cd98f00b204e9800998ecf8427e\"",
                "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
            ),
        ] {
            let path = directory.path().join(format!("store-{index}"));
            let connection = Connection::open(path.clone(), None).await?;
            connection.create_bucket(request().bucket).await?;

            let created = connection
                .create_multipart_upload(CreateRequest {
                    bucket: request().source.bucket,
                    key: request().source.key,
                    expected_owner: None,
                    representation: Representation::default(),
                    metadata: UserMetadata::default(),
                    tags: None,
                    checksum: MultipartChecksum::new(
                        ChecksumAlgorithm::Sha256,
                        ChecksumType::Composite,
                    )?,
                })
                .await?;

            let upload = UploadRequest {
                bucket: request().source.bucket,
                key: request().source.key,
                upload_id: created.upload_id,
                expected_owner: None,
            };

            let part_number = PartNumber::new(1)?;

            let part = connection
                .upload_part(
                    PartRequest {
                        bucket: upload.bucket.clone(),
                        key: upload.key.clone(),
                        upload_id: upload.upload_id.clone(),
                        part_number,
                        expected_owner: None,
                        content_length: None,
                        checksum_algorithm: None,
                        expected: ExpectedDigests::default(),
                    },
                    payload,
                )
                .await?;

            let completed = connection
                .complete_multipart_upload(CompleteRequest {
                    upload,
                    parts: CompletionParts::new([SelectedPart {
                        part_number,
                        etag: part.etag(),
                        checksum: Some(part.checksum()),
                    }])?,
                    conditions: WriteConditions::default(),
                    content_length: None,
                    checksum_type: None,
                    checksum: None,
                })
                .await?;

            assert_ne!(completed.etag, etag);
            let copied = connection.copy_object(request()).await?;
            assert_eq!(copied.facts.etag(), etag);
            assert_eq!(copied.facts.checksum().encode(), checksum);
            assert_eq!(bytes(&connection, "destination").await?, payload);

            connection
                .delete_object(objects::read("source"), None)
                .await?;

            connection.close().await?;

            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    // Inspect only with every engine handle closed. Reopening
                    // gives ordinary maintenance another bounded work interval.
                    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

                    let pending: i64 = database.query_row(
                        "SELECT (SELECT count(*) FROM blobs WHERE gc_pending=1)
                         + (SELECT count(*) FROM multipart_receipts)",
                        [],
                        |row| row.get(0),
                    )?;

                    database.close().map_err(|(_, error)| error)?;

                    if pending == 0 {
                        break;
                    }

                    let maintenance = Connection::open(path.clone(), None).await?;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    maintenance.close().await?;
                }

                Ok::<_, Box<dyn std::error::Error>>(())
            })
            .await??;

            let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

            assert_eq!(
                database.query_row("SELECT count(*) FROM blob_parts", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                0
            );

            assert_eq!(
                database.query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))?,
                1
            );

            database.close().map_err(|(_, error)| error)?;
            let reopened = Connection::open(path, None).await?;
            assert_eq!(bytes(&reopened, "destination").await?, payload);
            reopened.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
