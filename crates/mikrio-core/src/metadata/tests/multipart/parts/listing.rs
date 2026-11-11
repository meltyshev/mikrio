use super::*;
use crate::multipart::{ListPartsRequest, PartPageSize};

fn page(part: &PartRequest, marker: u32, max: u32) -> ListPartsRequest {
    ListPartsRequest {
        upload: super::abort::control(part),
        part_number_marker: marker,
        max_parts: PartPageSize::new(max).unwrap(),
    }
}

#[test]
fn a_thousand_part_page_has_one_lookahead_and_a_live_final_page() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let mut request = part(created.upload_id);
    let payload = facts(ChecksumAlgorithm::Sha256);

    // Metadata producer coverage, not a 1,001-file durability/capacity run.
    for number in 1_u32..=1001 {
        request.part_number = PartNumber::new(number)?;
        let mut bytes = [0; 16];
        bytes[..4].copy_from_slice(&number.to_be_bytes());
        let blob = BlobId::from_bytes(bytes);

        let reservation = database
            .reserve_part(&Authority::Native, &request, blob)?
            .unwrap();

        database.publish_part(&Authority::Native, &request, reservation, blob, &payload)?;
    }

    let first = database.list_parts(&Authority::Native, &page(&request, 0, 1000))?;
    assert_eq!(first.parts.len(), 1000);
    assert_eq!(first.parts.first().unwrap().part_number.get(), 1);
    assert_eq!(first.parts.last().unwrap().part_number.get(), 1000);
    assert_eq!(first.next_part_number_marker, Some(1000));
    let last = database.list_parts(&Authority::Native, &page(&request, 1000, 1000))?;
    assert_eq!(last.parts.len(), 1);
    assert_eq!(last.parts[0].part_number.get(), 1001);
    assert_eq!(last.next_part_number_marker, None);

    let mut query = database.connection.prepare(
        "SELECT part_number FROM multipart_parts WHERE upload_id = ?1
         AND part_number > ?2 ORDER BY part_number LIMIT 1001",
    )?;

    let numbers = query
        .query_map(params![request.upload_id.to_string(), 1000], |row| {
            row.get::<_, i64>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(numbers, [1001]);
    assert_eq!(query.get_status(rusqlite::StatementStatus::FullscanStep), 0);
    assert_eq!(query.get_status(rusqlite::StatementStatus::Sort), 0);
    assert!(query.get_status(rusqlite::StatementStatus::VmStep) < 100);
    drop(query);
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn part_pages_use_exclusive_numeric_boundaries_and_current_detached_facts() -> TestResult {
    assert_eq!(PartPageSize::default().get(), 1000);
    assert!(PartPageSize::new(1001).is_err());
    assert!(PartPageSize::new(u32::MAX).is_err());
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let mut request = part(created.upload_id);

    assert_eq!(
        database
            .list_parts(&Authority::Native, &page(&request, 0, 1000))?
            .parts,
        Vec::new(),
    );

    for (number, id) in [(1, 1), (7, 2), (10_000, 3)] {
        request.part_number = PartNumber::new(number)?;
        let (reservation, blob) = reserve(&mut database, &request, id)?;

        database.publish_part(
            &Authority::Native,
            &request,
            reservation,
            blob,
            &facts(ChecksumAlgorithm::Sha256),
        )?;
    }

    for (marker, max, numbers, next) in [
        (0, 0, vec![], None),
        (0, 2, vec![1, 7], Some(7)),
        (1, 2, vec![7, 10_000], None),
        (2, 1, vec![7], Some(7)),
        (7, 1000, vec![10_000], None),
        (10_000, 1000, vec![], None),
        (u32::MAX, 1000, vec![], None),
    ] {
        let result = database.list_parts(&Authority::Native, &page(&request, marker, max))?;

        assert_eq!(
            result
                .parts
                .iter()
                .map(|part| part.part_number.get())
                .collect::<Vec<_>>(),
            numbers,
            "marker={marker}, max_parts={max}"
        );

        assert_eq!(
            result.next_part_number_marker, next,
            "marker={marker}, max_parts={max}"
        );

        assert_eq!(result.initiator, database.get_owner_info()?.canonical_id());
        assert_eq!(result.checksum, created.checksum);

        for part in result.parts {
            assert_eq!(part.size.get(), 3);
            assert_eq!(part.etag, "\"900150983cd24fb0d6963f7d28e17f72\"");

            assert_eq!(
                part.checksum.encode(),
                "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="
            );
        }
    }

    let detached = database.list_parts(&Authority::Native, &page(&request, 7, 1))?;
    let (reservation, blob) = reserve(&mut database, &request, 4)?;

    let verifier = PayloadVerifier::new(
        None,
        PART_MAX_BYTES,
        ChecksumAlgorithm::Sha256,
        ExpectedDigests::default(),
    )?;

    database.publish_part(
        &Authority::Native,
        &request,
        reservation,
        blob,
        &verifier.finish(ExpectedDigests::default())?,
    )?;

    let replaced = database.list_parts(&Authority::Native, &page(&request, 7, 1))?;
    assert_eq!(replaced.parts[0].size.get(), 0);
    assert_eq!(detached.parts[0].size.get(), 3);
    assert_ne!(replaced.parts[0].etag, detached.parts[0].etag);
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(
        reopened
            .list_parts(&Authority::Native, &page(&request, 7, 1))?
            .parts,
        replaced.parts
    );

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn part_pages_validate_the_lookahead_and_scope_before_returning_a_page() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let created = database.create_multipart_upload(&Authority::Native, &request())?;
    let mut request = part(created.upload_id);

    for id in 1..=2 {
        request.part_number = PartNumber::new(u32::from(id))?;
        let (reservation, blob) = reserve(&mut database, &request, id)?;

        database.publish_part(
            &Authority::Native,
            &request,
            reservation,
            blob,
            &facts(ChecksumAlgorithm::Sha256),
        )?;
    }

    database.connection.execute(
        "UPDATE blobs SET checksums_json = '{}' WHERE blob_id = ?1",
        [[2_u8; 16].as_slice()],
    )?;

    assert_eq!(
        database
            .list_parts(&Authority::Native, &page(&request, 0, 1))
            .unwrap_err()
            .failure(),
        OperationFailure::StoreCorrupt
    );

    let mut wrong = page(&request, 0, 0);
    wrong.upload.key = ObjectKey::parse("wrong")?;

    assert_eq!(
        database
            .list_parts(&Authority::Native, &wrong)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchUpload
    );

    wrong.upload.key = request.key.clone();
    wrong.upload.expected_owner = Some("000000000000".into());

    assert_eq!(
        database
            .list_parts(&Authority::Native, &wrong)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn initiator_pages_do_not_transfer_to_a_reused_principal_name_or_bypass_denies() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let principal = database.create_principal(PrincipalName::new("uploader".into())?)?;
    let key = database.create_access_key(principal.principal_id)?;

    let identity = Authority::Http(Arc::new(
        database
            .resolve_application_key(key.access_key.access_key_id.as_str())?
            .verified(),
    ));

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject*","Resource":"*"}}"#,
    )?;

    let created = database.create_multipart_upload(&identity, &request())?;
    let request = page(&part(created.upload_id), 0, 1000);
    database.delete_principal_policy(principal.principal_id)?;
    assert_eq!(database.list_parts(&identity, &request)?.parts, Vec::new());

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Action":"s3:ListMultipartUploadParts","Resource":"*"}}"#,
    )?;

    assert_eq!(
        database
            .list_parts(&identity, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database.delete_access_key(principal.principal_id, &key.access_key.access_key_id)?;
    database.delete_principal(principal.principal_id)?;
    let replacement = database.create_principal(PrincipalName::new("uploader".into())?)?;
    let key = database.create_access_key(replacement.principal_id)?;

    let replacement = Authority::Http(Arc::new(
        database
            .resolve_application_key(key.access_key.access_key_id.as_str())?
            .verified(),
    ));

    for identity in [
        &identity,
        &replacement,
        &Authority::Http(Arc::new(HttpIdentity::anonymous())),
    ] {
        assert_eq!(
            database
                .list_parts(identity, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::AccessDenied
        );

        assert_eq!(
            database
                .abort_multipart_upload(identity, &request.upload)
                .unwrap_err()
                .failure(),
            OperationFailure::AccessDenied
        );
    }

    database.abort_multipart_upload(&Authority::Native, &request.upload)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}
