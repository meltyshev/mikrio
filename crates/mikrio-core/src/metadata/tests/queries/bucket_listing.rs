use super::*;
use crate::OperationFailure;
use crate::object::{Authority, BucketPageSize, ListBucketsRequest};

fn request() -> ListBucketsRequest {
    ListBucketsRequest {
        prefix: None,
        continuation_token: None,
        max_buckets: Some(BucketPageSize::default()),
        bucket_region: None,
    }
}

fn insert(database: &mut MetadataDatabase, names: &[&str]) -> TestResult {
    let transaction = database.connection.transaction()?;

    for name in names {
        transaction.execute(
            "INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES (randomblob(16), ?1, 1234)",
            [name],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

#[test]
fn literal_bucket_prefixes_and_page_bounds_match_live_names() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = seed(&directory, &[])?;
    let names = ["aaa", "aaa-b", "aaa.c", "bbb", "photos", "zzz"];
    insert(&mut database, &["aaa", "aaa-b", "aaa.c", "bbb", "zzz"])?;

    database.connection.execute(
        "INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES (randomblob(16), NULL, 0)",
        [],
    )?;

    for prefix in [
        None,
        Some(""),
        Some("a"),
        Some("aaa."),
        Some("z"),
        Some("Z"),
        Some("雪"),
        Some("../"),
        Some("%"),
        Some("a\0"),
    ] {
        let expected = names
            .iter()
            .filter(|name| name.starts_with(prefix.unwrap_or_default()))
            .copied()
            .collect::<Vec<_>>();

        for size in [1, 2, 5, 1000, 10_000] {
            let mut input = request();
            input.prefix = prefix.map(str::to_owned);
            input.max_buckets = Some(BucketPageSize::new(size)?);
            let mut actual = Vec::new();

            loop {
                let page = database.list_buckets(&Authority::Native, &input)?;
                assert!(page.buckets.len() <= usize::try_from(size)?);
                assert_eq!(page.bucket_region, "us-east-1");
                assert_eq!(page.canonical_owner_id.len(), 64);
                actual.extend(page.buckets.into_iter().map(|bucket| bucket.name));
                assert!(actual.len() <= names.len());
                input.continuation_token = page.continuation_token;

                if input.continuation_token.is_none() {
                    break;
                }
            }

            assert_eq!(actual, expected, "prefix={prefix:?} size={size}");
        }
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn bucket_tokens_preserve_presence_region_and_operation_scope() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = seed(&directory, &[])?;
    insert(&mut database, &["aaa", "bbb"])?;
    let mut input = request();
    input.max_buckets = Some(BucketPageSize::new(1)?);

    let token = database
        .list_buckets(&Authority::Native, &input)?
        .continuation_token
        .unwrap();

    for invalid in [
        String::new(),
        "!".into(),
        format!("{token}A"),
        token.get(..token.len() - 1).unwrap().into(),
        "A".repeat(100_000),
    ] {
        input.continuation_token = Some(invalid);

        assert_eq!(
            database
                .list_buckets(&Authority::Native, &input)
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidArgument
        );
    }

    input.continuation_token = Some(token.clone());
    input.prefix = Some(String::new());

    assert_eq!(
        database
            .list_buckets(&Authority::Native, &input)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    input.prefix = None;
    input.bucket_region = Some("us-east-1".into());

    assert_eq!(
        database
            .list_buckets(&Authority::Native, &input)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    input.bucket_region = None;

    assert_eq!(
        database
            .list_principals(crate::access::AdminPageSize::default(), Some(&token))
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    // Deletion and name reuse do not rewind a live lexical cursor.
    database
        .connection
        .execute("UPDATE buckets SET name=NULL WHERE name='aaa'", [])?;

    insert(&mut database, &["aaa", "ccc"])?;
    input.max_buckets = Some(BucketPageSize::new(10_000)?);
    let final_page = database.list_buckets(&Authority::Native, &input)?;

    assert_eq!(
        final_page
            .buckets
            .iter()
            .map(|bucket| bucket.name.as_str())
            .collect::<Vec<_>>(),
        ["bbb", "ccc", "photos"]
    );

    assert!(final_page.continuation_token.is_none());
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn corrupt_bucket_lookahead_fails_without_a_short_successful_page() -> TestResult {
    for corruption in [
        "UPDATE buckets SET name='bbb_BAD' WHERE name='bbb'",
        "UPDATE buckets SET created_at_ms=9223372036854775807 WHERE name='bbb'",
        "UPDATE buckets SET bucket_id=x'00' WHERE name='bbb'",
    ] {
        let directory = TestDirectory::new()?;
        let mut database = seed(&directory, &[])?;
        insert(&mut database, &["aaa", "bbb"])?;

        database
            .connection
            .pragma_update(None, "ignore_check_constraints", true)?;

        database
            .connection
            .pragma_update(None, "foreign_keys", false)?;

        database.connection.execute(corruption, [])?;
        let mut input = request();
        input.max_buckets = Some(BucketPageSize::new(1)?);

        assert_eq!(
            database
                .list_buckets(&Authority::Native, &input)
                .unwrap_err()
                .failure(),
            OperationFailure::StoreCorrupt,
            "{corruption}"
        );

        close(database)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
#[allow(
    clippy::print_stderr,
    reason = "retained metadata-only resource measurement"
)]
fn indexed_bucket_pages_and_complete_unpaginated_snapshot_at_one_hundred_thousand() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = seed(&directory, &[])?;
    let transaction = database.connection.transaction()?;

    {
        let mut statement = transaction
            .prepare("INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES (?1, ?2, 0)")?;

        for index in 0..100_000_u128 {
            statement.execute(params![index.to_be_bytes(), format!("bucket-{index:06}")])?;

            statement.execute(params![
                (index + 100_000).to_be_bytes(),
                Option::<String>::None
            ])?;
        }
    }

    transaction.commit()?;
    let mut input = request();
    input.prefix = Some("bucket-09999".into());
    input.max_buckets = Some(BucketPageSize::new(5)?);
    let started = Instant::now();
    let first = database.list_buckets(&Authority::Native, &input)?;
    input.continuation_token = first.continuation_token;
    let last = database.list_buckets(&Authority::Native, &input)?;
    assert_eq!(first.buckets.len(), 5);
    assert_eq!(last.buckets.len(), 5);
    assert!(last.continuation_token.is_none());
    let steps = crate::metadata::bucket_listing::indexed_work(&database.connection)?;
    assert!(steps > 0 && steps < 1000, "unexpected page work: {steps}");
    let page_us = started.elapsed().as_micros();

    let mut input = request();
    input.max_buckets = None;
    let started = Instant::now();
    let all = database.list_buckets(&Authority::Native, &input)?;
    assert_eq!(all.buckets.len(), 100_001);
    assert!(all.continuation_token.is_none());

    let owned_bytes = all.buckets.capacity() * size_of::<crate::object::ListedBucket>()
        + all
            .buckets
            .iter()
            .map(|bucket| bucket.name.capacity())
            .sum::<usize>()
        + all.bucket_region.capacity()
        + all.canonical_owner_id.capacity();

    eprintln!(
        "BUCKET_LIST_MEASUREMENT live=100001 retired=100000 page_vm_steps={steps} page_us={page_us} unpaginated_us={} detached_owned_bytes={owned_bytes}",
        started.elapsed().as_micros()
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
