use super::*;
use crate::integrity::{Checksum, ObjectChecksum};

async fn wait_for_completion_recovery(path: &std::path::Path) -> TestResult {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

            let completing: i64 = database.query_row(
                "SELECT count(*) FROM multipart_uploads WHERE state='completing'",
                [],
                |row| row.get(0),
            )?;

            database.close().map_err(|(_, error)| error)?;

            if completing == 0 {
                break;
            }

            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await?
}

#[test]
fn rejected_final_checksum_and_destination_condition_preserve_object_and_retryable_parts()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;

        for failure in [
            OperationFailure::BadDigest,
            OperationFailure::PreconditionFailed,
        ] {
            let previous = connection
                .put_object(
                    super::super::super::objects::put(request().key.as_str()),
                    b"before".as_slice(),
                )
                .await?;

            let created = connection.create_multipart_upload(request()).await?;

            connection
                .upload_part(
                    super::super::parts::part(created.upload_id.clone(), 1),
                    b"abc".as_slice(),
                )
                .await?;

            let input = completion(&connection, created.upload_id.clone()).await?;
            let mut rejected = input.clone();

            if failure == OperationFailure::BadDigest {
                rejected.checksum = Some(ObjectChecksum::full(Checksum::Crc64Nvme([0; 8])));
            } else {
                rejected.conditions = WriteConditions::new(None, Some("*".into()))?;
            }

            let error = connection
                .complete_multipart_upload(rejected)
                .await
                .unwrap_err();

            assert_eq!(error.failure(), failure);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
            assert_eq!(bytes(&connection).await?, b"before");
            wait_for_completion_recovery(&path).await?;

            assert_eq!(
                completion(&connection, created.upload_id)
                    .await?
                    .parts
                    .parts()[0]
                    .etag,
                input.parts.parts()[0].etag
            );

            let mut retry = input;
            retry.conditions = WriteConditions::new(Some(previous.etag()), None)?;
            connection.complete_multipart_upload(retry).await?;
            assert_eq!(bytes(&connection).await?, b"abc");
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;

    Ok(())
}

#[test]
fn same_length_corruption_in_a_selected_part_cannot_publish_or_hide_behind_recorded_digests()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    let upload_id = runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::super::objects::put(request().key.as_str()),
                b"before".as_slice(),
            )
            .await?;

        let created = connection.create_multipart_upload(request()).await?;

        connection
            .upload_part(
                super::super::parts::part(created.upload_id.clone(), 1),
                b"abc".as_slice(),
            )
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(created.upload_id)
    })?;

    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let blob: Vec<u8> =
        database.query_row("SELECT blob_id FROM multipart_parts", [], |row| row.get(0))?;

    database.close().map_err(|(_, error)| error)?;
    let blob = crate::BlobId::from_bytes(blob.try_into().map_err(|_| "invalid fixture blob")?);

    std::fs::write(
        path.join("objects")
            .join(format!("{:02x}", blob.shard()))
            .join(blob.to_string()),
        b"def",
    )?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        let input = completion(&connection, upload_id).await?;

        let error = connection
            .complete_multipart_upload(input)
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        assert_eq!(bytes(&connection).await?, b"before");
        wait_for_completion_recovery(&path).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;

    Ok(())
}
