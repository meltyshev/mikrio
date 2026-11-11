use super::*;

#[test]
fn rejected_owners_and_corrupt_source_bytes_preserve_the_previous_part() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    let (upload_id, source, previous) = runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::super::objects::put("source"),
                b"abc".as_slice(),
            )
            .await?;

        let created = connection.create_multipart_upload(request()).await?;

        connection
            .upload_part(
                super::super::parts::part(created.upload_id.clone(), 1),
                b"before".as_slice(),
            )
            .await?;

        let previous = page(&connection, created.upload_id.clone()).await?.parts;
        let owner = connection.get_owner_info().await?;

        let wrong = if owner.account_id() == "000000000000" {
            "111111111111"
        } else {
            "000000000000"
        };

        for source_owner in [false, true] {
            let mut copy = input(created.upload_id.clone(), 1);

            if source_owner {
                copy.expected_source_owner = Some(wrong.into());
            } else {
                copy.expected_owner = Some(wrong.into());
            }

            let error = connection.upload_part_copy(copy).await.unwrap_err();
            assert_eq!(error.failure(), OperationFailure::AccessDenied);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

            assert_eq!(
                page(&connection, created.upload_id.clone()).await?.parts,
                previous
            );
        }

        let source = connection
            .head_object(super::super::super::objects::read("source"))
            .await?;

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>((created.upload_id, source, previous))
    })?;

    let payload = path
        .join("objects")
        .join(format!("{:02x}", source.blob_id.shard()))
        .join(source.blob_id.to_string());

    for bytes in [b"def".as_slice(), b"ab".as_slice()] {
        // Modify only after every engine handle has closed.
        std::fs::write(&payload, bytes)?;

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;

            let error = connection
                .upload_part_copy(input(upload_id.clone(), 1))
                .await
                .unwrap_err();

            assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
            assert_eq!(page(&connection, upload_id.clone()).await?.parts, previous);
            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        complete(&connection, upload_id).await?;

        let (_, mut body) = connection
            .get_object(super::super::super::objects::read(request().key.as_str()))
            .await?;

        assert_eq!(body.read_all(6).await?, b"before");
        body.close().await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn ranged_copy_checks_full_length_without_scanning_unselected_corruption() -> TestResult {
    use crate::multipart::CopyRange;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let mut bytes = vec![b'x'; 5 * 1024 * 1024 + 1];
    bytes[65_535..65_538].copy_from_slice(b"abc");

    let (upload_id, source) = runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::super::objects::put("source"),
                std::io::Cursor::new(bytes.clone()),
            )
            .await?;

        let created = connection.create_multipart_upload(request()).await?;

        let source = connection
            .head_object(super::super::super::objects::read("source"))
            .await?;

        connection.close().await?;

        Ok::<_, Box<dyn std::error::Error>>((created.upload_id, source))
    })?;

    let payload = path
        .join("objects")
        .join(format!("{:02x}", source.blob_id.shard()))
        .join(source.blob_id.to_string());

    bytes[0] = b'y';

    std::fs::write(&payload, &bytes)?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        let mut selected = input(upload_id.clone(), 1);
        selected.source_range = Some(CopyRange::parse("bytes=65535-65537")?);
        let copied = connection.upload_part_copy(selected).await?;
        assert_eq!(copied.facts.etag(), "\"900150983cd24fb0d6963f7d28e17f72\"");

        let error = connection
            .upload_part_copy(input(upload_id.clone(), 1))
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);

        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

        assert_eq!(
            page(&connection, upload_id.clone()).await?.parts[0].etag,
            copied.facts.etag()
        );

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    bytes.pop();
    std::fs::write(&payload, &bytes)?;

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        let mut selected = input(upload_id.clone(), 1);
        selected.source_range = Some(CopyRange::parse("bytes=65535-65537")?);
        let error = connection.upload_part_copy(selected).await.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        complete(&connection, upload_id).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
