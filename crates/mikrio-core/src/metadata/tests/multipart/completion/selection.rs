use super::*;

#[test]
fn sparse_selection_skips_unrequested_corruption_and_rejects_missing_requested_parts() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let mut initiation = request();

    initiation.checksum =
        MultipartChecksum::new(ChecksumAlgorithm::Crc64Nvme, ChecksumType::FullObject)?;

    let upload = database.create_multipart_upload(&Authority::Native, &initiation)?;
    let facts = super::super::parts::facts(ChecksumAlgorithm::Crc64Nvme);

    for number in 1..=5 {
        let mut part = super::super::parts::part(upload.upload_id.clone());
        part.part_number = PartNumber::new(number)?;

        let (reservation, blob) =
            super::super::parts::reserve(&mut database, &part, u8::try_from(number)?)?;

        database.publish_part(&Authority::Native, &part, reservation, blob, &facts)?;
    }

    // Synthetic metadata exercises selection only, without allocating large files.
    database
        .connection
        .execute("UPDATE blobs SET byte_length = 5242880", [])?;

    database.connection.execute(
        "UPDATE blobs SET checksums_json = '{}' WHERE blob_id = ?1",
        [[3_u8; 16].as_slice()],
    )?;

    let mut request = CompleteRequest {
        upload: UploadRequest {
            bucket: initiation.bucket,
            key: initiation.key,
            upload_id: upload.upload_id,
            expected_owner: None,
        },
        parts: CompletionParts::new([1, 2, 4, 5].map(selected))?,
        conditions: WriteConditions::default(),
        content_length: None,
        checksum_type: None,
        checksum: None,
    };

    database.connection.flush_prepared_statement_cache();
    let selection = database.select_completion(&Authority::Native, &request)?;
    let runs = crate::metadata::multipart::part_selection_work(&database.connection)?;

    assert!(
        (1..=2).contains(&runs),
        "sparse selection ran {runs} part queries"
    );

    assert_eq!(
        selection
            .parts
            .iter()
            .map(|part| (part.part_number.get(), *part.blob_id.as_bytes()))
            .collect::<Vec<_>>(),
        vec![(1, [1; 16]), (2, [2; 16]), (4, [4; 16]), (5, [5; 16])]
    );

    assert_eq!(selection.layout.content_length(), 4 * 5 * 1024 * 1024);

    request.parts = CompletionParts::new([selected(3)])?;

    assert_eq!(
        database
            .select_completion(&Authority::Native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::StoreCorrupt
    );

    for (missing, requested) in [(2, vec![1, 2, 3, 4, 5]), (5, vec![1, 4, 5])] {
        database.connection.execute(
            "DELETE FROM multipart_parts WHERE upload_id = ?1 AND part_number = ?2",
            rusqlite::params![request.upload.upload_id.to_string(), missing],
        )?;

        request.parts = CompletionParts::new(requested.into_iter().map(selected))?;

        assert_eq!(
            database
                .select_completion(&Authority::Native, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidPart,
            "missing part {missing}"
        );
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}
