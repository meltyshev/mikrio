use super::*;
use crate::object::{Attribute, AttributeSelection, AttributesRequest, ObjectTags};
use crate::test_backup::{copy_directory, manifest};

mod authorization;

fn attributes() -> AttributesRequest {
    AttributesRequest {
        bucket: request().bucket,
        key: request().key,
        expected_owner: None,
        attributes: AttributeSelection::new([
            Attribute::Etag,
            Attribute::Checksum,
            Attribute::ObjectParts,
            Attribute::StorageClass,
            Attribute::ObjectSize,
        ])
        .unwrap(),
        max_parts: PartPageSize::new(1).unwrap(),
        part_number_marker: 0,
    }
}

#[test]
fn attribute_pages_survive_original_part_collection_retag_and_reopen() -> TestResult {
    retained_layout(false)
}

#[test]
fn composite_attribute_pages_survive_copied_part_collection_and_restore() -> TestResult {
    retained_layout(true)
}

fn retained_layout(composite: bool) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;
        let mut source = vec![b'a'; 5 * 1024 * 1024];
        source.extend_from_slice(b"tail");

        connection
            .put_object(
                super::super::super::objects::put("source"),
                std::io::Cursor::new(source),
            )
            .await?;

        let mut initiation = request();

        if composite {
            initiation.checksum =
                MultipartChecksum::new(ChecksumAlgorithm::Sha256, ChecksumType::Composite)?;
        }

        let created = connection.create_multipart_upload(initiation).await?;
        let (first_number, last_number) = if composite { (1, 2) } else { (2, 7) };
        let mut copy = super::super::copy::input(created.upload_id.clone(), first_number);
        copy.source_range = Some(crate::multipart::CopyRange::parse("bytes=0-5242879")?);
        connection.upload_part_copy(copy).await?;

        connection
            .upload_part(
                super::super::parts::part(created.upload_id.clone(), last_number),
                b"tail".as_slice(),
            )
            .await?;

        let input = completion(&connection, created.upload_id).await?;
        let completed = connection.complete_multipart_upload(input).await?;
        connection.close().await?;

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                // Inspect only with all engine handles closed.
                let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

                let pending: i64 = database.query_row(
                    "SELECT (SELECT count(*) FROM blobs WHERE gc_pending=1)
                     + (SELECT count(*) FROM multipart_receipts)",
                    [],
                    |row| row.get(0),
                )?;

                database.close().map_err(|(_, error)| error)?;

                if pending == 0 {
                    return Ok::<_, Box<dyn std::error::Error>>(());
                }

                let worker = Connection::open(path.clone(), None).await?;
                tokio::time::sleep(Duration::from_millis(20)).await;
                worker.close().await?;
            }
        })
        .await??;

        let connection = Connection::open(path.clone(), None).await?;
        let page = connection.get_object_attributes(attributes()).await?;
        assert_eq!(page.etag, Some(completed.etag.clone()));
        assert_eq!(page.object_size.unwrap().get(), 5 * 1024 * 1024 + 4);
        let modified = page.last_modified;
        let page = page.object_parts.unwrap();
        assert_eq!(page.total_parts_count, 2);
        assert_eq!(page.entries_available, composite);
        assert_eq!(page.next_part_number_marker, composite.then_some(1));

        if composite {
            assert_eq!(page.parts.len(), 1);
            assert_eq!(page.parts[0].part_number, 1);

            assert_eq!(
                page.parts[0].checksums[0].encode(),
                "oplo+tLngqqfIECjXwWtuX7Yl56x9XLIyOp4Y34nXzw="
            );
        } else {
            assert_eq!(page.parts, []);
        }

        assert_part(&connection, 1, 5 * 1024 * 1024, b"a", composite).await?;

        connection
            .replace_object_tags(
                super::super::super::objects::read(request().key.as_str()),
                ObjectTags::new([("phase".into(), "retagged".into())])?,
            )
            .await?;

        let mut next = attributes();
        next.part_number_marker = first_number;
        let page = connection.get_object_attributes(next.clone()).await?;
        assert_eq!(page.last_modified, modified);
        assert_eq!(page.etag, Some(completed.etag));
        let page = page.object_parts.unwrap();
        assert_eq!(page.next_part_number_marker, None);
        assert_eq!(page.entries_available, composite);

        if composite {
            assert_eq!(page.parts.len(), 1);
            assert_eq!(page.parts[0].part_number, 2);

            assert_eq!(
                page.parts[0].checksums[0].encode(),
                "DGL4du8d6oMN6fMsL0tG3W101Q0ViW4J71ovzUrH4dc="
            );
        } else {
            assert_eq!(page.parts, []);
        }

        assert_part(&connection, 2, 4, b"tail", composite).await?;
        let identity = connection.info().record.clone();
        connection.close().await?;

        let backup = directory.path().join("backup");
        let working = directory.path().join("restored");
        let frozen = manifest(&path)?;
        copy_directory(&path, &backup)?;
        assert_eq!(manifest(&backup)?, frozen);
        copy_directory(&backup, &working)?;
        assert_eq!(manifest(&working)?, frozen);

        let connection = Connection::open(working, None).await?;
        assert_eq!(connection.info().record, identity);
        let restored = connection.get_object_attributes(attributes()).await?;
        assert_eq!(restored.last_modified, modified);
        assert_eq!(restored.object_size.unwrap().get(), 5 * 1024 * 1024 + 4);

        assert_eq!(
            restored.etag.as_deref(),
            Some("\"30dcfd3901d1c613b7fb532281748544-2\"")
        );

        let restored = restored.object_parts.unwrap();
        assert_eq!(restored.total_parts_count, 2);
        assert_eq!(restored.entries_available, composite);
        assert_eq!(restored.parts.len(), usize::from(composite));
        assert_part(&connection, 1, 5 * 1024 * 1024, b"a", composite).await?;
        assert_part(&connection, 2, 4, b"tail", composite).await?;
        let payload = bytes(&connection).await?;
        verify_payload(&payload)?;

        connection
            .put_object(
                super::super::super::objects::put(request().key.as_str()),
                b"replacement".as_slice(),
            )
            .await?;

        let replacement = connection.get_object_attributes(next).await?;
        assert!(replacement.object_parts.is_none());
        assert_eq!(replacement.object_size.unwrap().get(), 11);
        connection.close().await?;

        assert_eq!(manifest(&backup)?, frozen);
        assert_eq!(manifest(&path)?, frozen);
        let original = Connection::open(path, None).await?;
        assert_eq!(bytes(&original).await?, payload);
        let original_parts = original.get_object_attributes(attributes()).await?;
        assert_eq!(original_parts.object_parts.unwrap().total_parts_count, 2);
        original.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

async fn assert_part(
    connection: &Connection,
    number: u32,
    length: u64,
    first: &[u8],
    composite: bool,
) -> TestResult {
    let mut selected = super::super::super::objects::read(request().key.as_str());
    selected.part_number = Some(crate::multipart::PartNumber::new(number)?);
    let head = connection.head_object(selected.clone()).await?;
    assert_eq!(head.content_length(), length);
    assert_eq!(head.parts_count(), Some(2));

    if composite {
        assert_eq!(head.checksum_type, Some(ChecksumType::Composite));

        assert_eq!(
            head.checksums["SHA256"],
            if number == 1 {
                "oplo+tLngqqfIECjXwWtuX7Yl56x9XLIyOp4Y34nXzw="
            } else {
                "DGL4du8d6oMN6fMsL0tG3W101Q0ViW4J71ovzUrH4dc="
            }
        );
    } else {
        assert!(head.checksums.is_empty());
    }

    let (metadata, mut body) = connection.get_object(selected).await?;
    assert_eq!(metadata.content_range(), head.content_range());
    let bytes = body.read_all(usize::try_from(length)?).await?;
    assert_eq!(bytes.len(), usize::try_from(length)?);
    assert_eq!(bytes, first.repeat(usize::try_from(length)? / first.len()));
    body.close().await?;
    Ok(())
}

fn verify_payload(payload: &[u8]) -> TestResult {
    use sha2::Digest as _;

    let expected = crate::values::decode_hex::<32>(
        b"d15a55813872a596ca56a0d2119230fbce8dc3c0ccb36fde29fd36b6d764bb7f",
    )?;

    assert_eq!(sha2::Sha256::digest(payload).as_slice(), expected);
    Ok(())
}

#[test]
fn attribute_selection_never_opens_the_payload_and_returns_detached_facts() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    let blob = runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::super::objects::put(request().key.as_str()),
                b"abc".as_slice(),
            )
            .await?;

        let object = connection
            .head_object(super::super::super::objects::read(request().key.as_str()))
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(object.blob_id)
    })?;

    std::fs::remove_file(
        path.join("objects")
            .join(format!("{:02x}", blob.shard()))
            .join(blob.to_string()),
    )?;

    let result = runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        let selected = connection.get_object_attributes(attributes()).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(selected)
    })?;

    assert_eq!(
        result.etag.as_deref(),
        Some("\"900150983cd24fb0d6963f7d28e17f72\"")
    );

    assert_eq!(result.object_size.unwrap().get(), 3);
    assert_eq!(result.checksum.unwrap().1["CRC64NVME"], "BeXKuz/B+us=");
    assert!(result.object_parts.is_none());
    directory.finish()?;
    Ok(())
}
