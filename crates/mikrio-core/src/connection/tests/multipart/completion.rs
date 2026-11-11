use super::*;
use crate::multipart::{
    CompleteRequest, CompletionParts, ListPartsRequest, PartPageSize, SelectedPart, UploadRequest,
};

use crate::object::WriteConditions;

mod attributes;
mod measurement;
mod outcomes;
mod rejection;

fn upload(upload_id: UploadId) -> UploadRequest {
    UploadRequest {
        bucket: request().bucket,
        key: request().key,
        upload_id,
        expected_owner: None,
    }
}

async fn completion(
    connection: &Connection,
    upload_id: UploadId,
) -> Result<CompleteRequest, Box<dyn std::error::Error>> {
    let page = connection
        .list_parts(ListPartsRequest {
            upload: upload(upload_id.clone()),
            part_number_marker: 0,
            max_parts: PartPageSize::default(),
        })
        .await?;

    Ok(CompleteRequest {
        upload: upload(upload_id),
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
}

async fn bytes(connection: &Connection) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let (_, mut body) = connection
        .get_object(super::super::objects::read(request().key.as_str()))
        .await?;

    let mut bytes = Vec::new();

    while let Some(chunk) = body.read_chunk(64 * 1024).await? {
        bytes.extend_from_slice(&chunk);
    }

    body.close().await?;
    Ok(bytes)
}

#[test]
fn sparse_selected_bytes_and_layout_survive_unused_part_and_receipt_collection_and_reopen()
-> TestResult {
    use sha2::Digest as _;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;
        let created = connection.create_multipart_upload(request()).await?;

        connection
            .upload_part(
                super::parts::part(created.upload_id.clone(), 1),
                std::io::Cursor::new(vec![b'a'; 5 * 1024 * 1024]),
            )
            .await?;

        connection
            .upload_part(
                super::parts::part(created.upload_id.clone(), 2),
                b"unused".as_slice(),
            )
            .await?;

        connection
            .upload_part(
                super::parts::part(created.upload_id.clone(), 7),
                b"tail".as_slice(),
            )
            .await?;

        let mut input = completion(&connection, created.upload_id.clone()).await?;

        input.parts = CompletionParts::new(
            input
                .parts
                .parts()
                .iter()
                .filter(|part| part.part_number.get() != 2)
                .cloned(),
        )?;

        let result = connection.complete_multipart_upload(input).await?;
        assert_eq!(result.etag, "\"30dcfd3901d1c613b7fb532281748544-2\"");
        assert_eq!(result.length.get(), 5 * 1024 * 1024 + 4);
        let payload = bytes(&connection).await?;

        let expected = crate::values::decode_hex::<32>(
            b"d15a55813872a596ca56a0d2119230fbce8dc3c0ccb36fde29fd36b6d764bb7f",
        )?;

        assert_eq!(sha2::Sha256::digest(&payload).as_slice(), expected);

        assert_eq!(
            connection
                .abort_multipart_upload(upload(created.upload_id))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchUpload
        );

        connection
            .owner
            .state
            .context
            .test_hooks
            .pause_maintenance
            .store(false, Ordering::SeqCst);

        connection.owner.state.context.wake_maintenance.notify_one();
        connection.close().await?;

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
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

                let worker = Connection::open(path.clone(), None).await?;
                tokio::time::sleep(Duration::from_millis(20)).await;
                worker.close().await?;
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await??;

        let reopened = Connection::open(path.clone(), None).await?;
        assert_eq!(bytes(&reopened).await?, payload);

        let page = reopened
            .list_objects_v2(crate::object::ListObjectsRequest {
                bucket: request().bucket,
                expected_owner: None,
                prefix: None,
                delimiter: None,
                start_after: None,
                continuation_token: None,
                max_keys: crate::object::ObjectPageSize::default(),
            })
            .await?;

        assert_eq!(page.contents.len(), 1);
        assert_eq!(page.contents[0].key, "durable/initiation");

        assert_eq!(
            page.contents[0].etag,
            "\"30dcfd3901d1c613b7fb532281748544-2\""
        );

        assert_eq!(page.contents[0].size, 5 * 1024 * 1024 + 4);
        assert!(page.next_continuation_token.is_none());
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let mut query = database.prepare(
        "SELECT part_number,byte_offset,byte_length FROM blob_parts ORDER BY part_number",
    )?;

    let layout = query
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(
        layout,
        vec![(1, 0, 5 * 1024 * 1024), (7, 5 * 1024 * 1024, 4)]
    );

    drop(query);

    assert_eq!(
        database.query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))?,
        1
    );

    database.close().map_err(|(_, error)| error)?;
    directory.finish()?;

    Ok(())
}

#[test]
fn empty_and_all_supported_completion_checksum_pairs_match_independent_vectors() -> TestResult {
    #[derive(serde::Deserialize)]
    struct Vector {
        algorithm: String,
        mode: String,
        body: String,
        etag: String,
        checksum: String,
    }

    #[derive(serde::Deserialize)]
    struct Fixture {
        vectors: Vec<Vector>,
    }

    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../../../../tests/contract/multipart-completion.json"
    ))?;

    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.create_bucket(request().bucket).await?;

        for vector in fixture.vectors {
            let algorithm = ChecksumAlgorithm::parse(&vector.algorithm)?;

            let mode = match vector.mode.as_str() {
                "FULL_OBJECT" => ChecksumType::FullObject,
                "COMPOSITE" => ChecksumType::Composite,
                _ => return Err("invalid independent fixture mode".into()),
            };

            let mut initiation = request();
            initiation.checksum = MultipartChecksum::new(algorithm, mode)?;
            let created = connection.create_multipart_upload(initiation).await?;

            connection
                .upload_part(
                    super::parts::part(created.upload_id.clone(), 1),
                    std::io::Cursor::new(vector.body.clone().into_bytes()),
                )
                .await?;

            let mut input = completion(&connection, created.upload_id).await?;

            input.checksum = Some(crate::integrity::ObjectChecksum::parse(
                algorithm,
                mode,
                &vector.checksum,
            )?);

            let result = connection.complete_multipart_upload(input).await?;
            assert_eq!(result.etag, vector.etag);
            assert_eq!(result.checksum.encode(), vector.checksum);
            assert_eq!(bytes(&connection).await?, vector.body.as_bytes());
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;

    Ok(())
}
