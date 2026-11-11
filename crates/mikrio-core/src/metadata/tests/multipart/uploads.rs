use super::*;
use crate::multipart::{ListUploadsRequest, UploadPageSize, UploadRequest};
use std::io::Write as _;

fn page() -> ListUploadsRequest {
    ListUploadsRequest {
        bucket: request().bucket,
        expected_owner: None,
        prefix: None,
        delimiter: None,
        key_marker: None,
        upload_id_marker: None,
        max_uploads: UploadPageSize::default(),
    }
}

fn initiate(
    database: &mut MetadataDatabase,
    key: &str,
    time_ms: i64,
    suffix: u8,
) -> Result<UploadId, Box<dyn std::error::Error>> {
    let mut request = request();
    request.key = ObjectKey::parse(key)?;
    let upload_id = UploadId::new(EpochMillis::new(time_ms)?, [suffix; 16]);
    database.initiate_multipart(&Authority::Native, &request, upload_id.clone())?;
    Ok(upload_id)
}

#[test]
fn upload_pages_order_literal_keys_then_original_time_and_accept_arbitrary_markers() -> TestResult {
    assert_eq!(UploadPageSize::default().get(), 1000);

    for invalid in [0, 1001, u32::MAX] {
        assert!(
            UploadPageSize::new(invalid).is_err(),
            "max_uploads={invalid}"
        );
    }

    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let later = initiate(&mut database, "a/../+é", 1000, 2)?;
    let equal = initiate(&mut database, "a/../+é", 1000, 1)?;
    let earlier = initiate(&mut database, "a/../+é", -1, 9)?;
    let second = initiate(&mut database, "z", 0, 4)?;
    let request = page();
    let all = database.list_multipart_uploads(&Authority::Native, &request)?;

    assert_eq!(
        all.uploads
            .iter()
            .map(|upload| &upload.upload_id)
            .collect::<Vec<_>>(),
        [&earlier, &equal, &later, &second]
    );

    assert_eq!(all.uploads[0].key, "a/../+é");
    assert_eq!(all.next_marker, None);

    for (key, upload, expected) in [
        (
            None,
            Some("zzzz"),
            vec![
                earlier.clone(),
                equal.clone(),
                later.clone(),
                second.clone(),
            ],
        ),
        (Some("a/../+é"), None, vec![second.clone()]),
        (
            Some("a/../+é"),
            Some(""),
            vec![
                earlier.clone(),
                equal.clone(),
                later.clone(),
                second.clone(),
            ],
        ),
        (Some("a/../+é"), Some("zzzz"), vec![second.clone()]),
        (
            Some("a/../+é"),
            Some(equal.to_string().as_str()),
            vec![later.clone(), second.clone()],
        ),
        (
            Some("deleted/arbitrary"),
            Some("not-an-upload-id"),
            vec![second.clone()],
        ),
        (Some("z"), None, vec![]),
    ] {
        let mut request = page();
        request.key_marker = key.map(str::to_owned);
        request.upload_id_marker = upload.map(str::to_owned);
        let actual = database.list_multipart_uploads(&Authority::Native, &request)?;

        assert_eq!(
            actual
                .uploads
                .into_iter()
                .map(|upload| upload.upload_id)
                .collect::<Vec<_>>(),
            expected,
            "key_marker={key:?}, upload_id_marker={upload:?}"
        );
    }

    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(
        reopened
            .list_multipart_uploads(&Authority::Native, &request)?
            .uploads,
        all.uploads
    );

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn upload_delimiter_pages_seek_whole_groups_and_continue_after_a_deleted_marker() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;

    for (key, suffix) in [("a/one", 1), ("a/two", 2), ("a/", 3), ("b", 4), ("c/é", 5)] {
        initiate(&mut database, key, 0, suffix)?;
    }

    let mut request = page();
    request.delimiter = Some("/".into());
    request.max_uploads = UploadPageSize::new(1)?;
    let first = database.list_multipart_uploads(&Authority::Native, &request)?;
    assert_eq!(first.common_prefixes, ["a/"]);
    assert_eq!(first.uploads, Vec::new());
    let marker = first.next_marker.unwrap();
    assert_eq!(marker.key, "a/");
    assert_eq!(marker.upload_id, None);
    request.key_marker = Some(marker.key);
    request.upload_id_marker = marker.upload_id;
    let second = database.list_multipart_uploads(&Authority::Native, &request)?;
    assert_eq!(second.uploads.len(), 1);
    assert_eq!(second.uploads[0].key, "b");
    let marker = second.next_marker.unwrap();

    database.abort_multipart_upload(
        &Authority::Native,
        &UploadRequest {
            bucket: request.bucket.clone(),
            key: ObjectKey::parse("b")?,
            upload_id: second.uploads[0].upload_id.clone(),
            expected_owner: None,
        },
    )?;

    request.key_marker = Some(marker.key);
    request.upload_id_marker = marker.upload_id;
    let final_page = database.list_multipart_uploads(&Authority::Native, &request)?;
    assert_eq!(final_page.common_prefixes, ["c/"]);
    assert_eq!(final_page.next_marker, None);

    request.key_marker = Some("a/one".into());
    request.upload_id_marker = Some("arbitrary".into());

    assert_eq!(
        database
            .list_multipart_uploads(&Authority::Native, &request)?
            .common_prefixes,
        ["c/"]
    );

    request.prefix = Some("a/".into());
    request.key_marker = Some(String::new());
    request.upload_id_marker = Some("ignored-before-prefix".into());
    request.max_uploads = UploadPageSize::new(1000)?;
    let prefixed = database.list_multipart_uploads(&Authority::Native, &request)?;

    assert_eq!(
        prefixed
            .uploads
            .iter()
            .map(|upload| upload.key.as_str())
            .collect::<Vec<_>>(),
        ["a/", "a/one", "a/two"]
    );

    request.prefix = Some("a/../literal".into());

    assert_eq!(
        database
            .list_multipart_uploads(&Authority::Native, &request)?
            .uploads,
        Vec::new()
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn upload_listing_requires_current_bucket_permission_without_initiator_or_prefix_context()
-> TestResult {
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

    database.create_multipart_upload(&identity, &request())?;
    let mut request = page();
    request.prefix = Some("literal/".into());
    request.delimiter = Some("/".into());

    assert_eq!(
        database
            .list_multipart_uploads(&identity, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:ListBucketMultipartUploads","Resource":"arn:aws:s3:::attachments","Condition":{"Null":{"s3:prefix":true,"s3:delimiter":true}}}}"#,
    )?;

    assert_eq!(
        database
            .list_multipart_uploads(&identity, &request)?
            .common_prefixes,
        ["literal/../"]
    );

    database.set_principal_enabled(principal.principal_id, false)?;

    assert_eq!(
        database
            .list_multipart_uploads(&identity, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database.set_principal_enabled(principal.principal_id, true)?;
    database.delete_access_key(principal.principal_id, &key.access_key.access_key_id)?;

    assert_eq!(
        database
            .list_multipart_uploads(&identity, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    request.expected_owner = Some("000000000000".into());

    assert_eq!(
        database
            .list_multipart_uploads(&Authority::Native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn upload_listing_rejects_corrupt_history_including_the_lookahead() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    initiate(&mut database, "a", 0, 1)?;
    let second = initiate(&mut database, "b", 0, 2)?;
    let owner = database.get_owner_info()?.canonical_id().to_owned();
    let mut request = page();
    request.max_uploads = UploadPageSize::new(1)?;

    for sql in [
        "UPDATE multipart_uploads SET initiated_at_ms = 1 WHERE key = x'62'",
        "UPDATE multipart_uploads SET initiator_id = 'arn:aws:iam::000000000000:user/00000000000000000000000000000000' WHERE key = x'62'",
        "UPDATE multipart_uploads SET checksum_algorithm = 'SHA256', checksum_type = 'FULL_OBJECT' WHERE key = x'62'",
    ] {
        database.connection.execute_batch(sql)?;

        assert_eq!(
            database
                .list_multipart_uploads(&Authority::Native, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::StoreCorrupt,
            "corruption={sql}"
        );

        database.connection.execute(
            "UPDATE multipart_uploads SET initiated_at_ms = 0, initiator_id = ?1,
             checksum_algorithm = 'SHA256', checksum_type = 'COMPOSITE' WHERE upload_id = ?2",
            rusqlite::params![owner, second.to_string()],
        )?;
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn upload_marker_and_delimiter_queries_seek_dense_namespaces_without_scanning_them() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let owner = database.get_owner_info()?.canonical_id().to_owned();

    let bucket_id: Vec<u8> = database.connection.query_row(
        "SELECT bucket_id FROM buckets WHERE name = 'attachments'",
        [],
        |row| row.get(0),
    )?;

    // Synthetic metadata density only: no uploaded payloads or durability claim.
    let transaction = database.connection.transaction()?;

    {
        let mut statement = transaction.prepare(
            "INSERT INTO multipart_uploads (upload_id, bucket_id, key, initiator_id, state,
             checksum_algorithm, checksum_type, user_metadata_json, tags_json, initiated_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'open', 'SHA256', 'COMPOSITE', '{}', '{}', 0)",
        )?;

        for number in 1_u32..=20_000 {
            let mut bytes = [0; 16];
            bytes[..4].copy_from_slice(&number.to_be_bytes());
            let upload = UploadId::new(EpochMillis::new(0)?, bytes);
            let key = if number <= 10_000 { "a/dense" } else { "same" };

            statement.execute(rusqlite::params![
                upload.to_string(),
                bucket_id,
                key.as_bytes(),
                owner
            ])?;
        }
    }

    transaction.commit()?;
    let mut request = page();
    request.delimiter = Some("/".into());
    request.max_uploads = UploadPageSize::new(1)?;
    let grouped = database.list_multipart_uploads(&Authority::Native, &request)?;
    assert_eq!(grouped.common_prefixes, ["a/"]);
    assert_eq!(grouped.next_marker.unwrap().key, "a/");
    request.prefix = Some("s".into());
    request.delimiter = None;
    request.key_marker = Some("same".into());
    let mut bytes = [0; 16];
    bytes[..4].copy_from_slice(&19_998_u32.to_be_bytes());
    request.upload_id_marker = Some(UploadId::new(EpochMillis::new(0)?, bytes).to_string());
    request.max_uploads = UploadPageSize::new(1000)?;
    let tail = database.list_multipart_uploads(&Authority::Native, &request)?;
    assert_eq!(tail.uploads.len(), 2);
    assert_eq!(tail.next_marker, None);
    let (steps, _) = super::super::super::multipart::uploads::indexed_work(&database.connection)?;

    writeln!(
        std::io::stderr().lock(),
        "20,000-upload metadata fixture: {steps} traversal VM steps"
    )?;

    assert!(
        steps < 500,
        "traversal must seek over hidden groups and earlier same-key IDs: {steps}"
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn flat_delimiter_pages_batch_rows_without_per_upload_queries() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;

    for suffix in 0..65 {
        initiate(&mut database, &format!("key-{suffix:04}"), 0, suffix)?;
    }

    for prefix in [None, Some("key-".to_owned())] {
        let mut request = page();
        request.prefix = prefix;
        request.delimiter = Some("/".into());
        request.max_uploads = UploadPageSize::new(64)?;

        let (_, before) =
            super::super::super::multipart::uploads::indexed_work(&database.connection)?;

        let first = database.list_multipart_uploads(&Authority::Native, &request)?;

        let (_, after) =
            super::super::super::multipart::uploads::indexed_work(&database.connection)?;

        assert_eq!(first.uploads.len(), 64);
        assert_eq!(first.uploads[0].key, "key-0000");
        assert_eq!(first.uploads[63].key, "key-0063");
        assert_eq!(first.common_prefixes, Vec::<String>::new());

        assert!(
            after - before <= 2,
            "per-upload SQL queries: {}",
            after - before
        );

        let marker = first.next_marker.unwrap();
        request.key_marker = Some(marker.key);
        request.upload_id_marker = marker.upload_id;
        let last = database.list_multipart_uploads(&Authority::Native, &request)?;
        assert_eq!(last.uploads.len(), 1);
        assert_eq!(last.uploads[0].key, "key-0064");
        assert!(last.next_marker.is_none());
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}
