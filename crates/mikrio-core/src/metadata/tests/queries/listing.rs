//! Metadata-only adversarial pages exercise the production selector and decoder.

use super::*;
use crate::OperationFailure;
use crate::object::{Authority, BucketName, ListObjectsRequest, ObjectPageSize};

fn request() -> ListObjectsRequest {
    ListObjectsRequest {
        bucket: BucketName::parse("photos").unwrap(),
        expected_owner: None,
        prefix: None,
        delimiter: None,
        start_after: None,
        continuation_token: None,
        max_keys: ObjectPageSize::default(),
    }
}

#[test]
fn real_pages_match_independent_literal_grouping_across_page_sizes() -> TestResult {
    let directory = TestDirectory::new()?;

    let keys: Vec<String> = [
        "a",
        "a/b",
        "a//b",
        "a/./b",
        "a/../b",
        "a%2fb",
        "a+b",
        "a\0b",
        "e\u{301}",
        "é",
        "雪/a",
        "雪/b",
        "\u{10ffff}/a",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    let mut database = seed(&directory, &keys)?;

    for prefix in [
        None,
        Some(""),
        Some("a"),
        Some("a/"),
        Some("../"),
        Some("雪"),
    ] {
        for delimiter in [None, Some(""), Some("/"), Some("//"), Some("雪")] {
            for start_after in [None, Some(""), Some("a/"), Some("a/zz"), Some("é")] {
                // Deliberately materialized independent oracle, never used by
                // storage. Collapse groups before applying the lexical marker.
                let mut expected = std::collections::BTreeSet::new();

                for key in &keys {
                    let prefix = prefix.unwrap_or_default();

                    if !key.starts_with(prefix) {
                        continue;
                    }

                    let suffix = key.strip_prefix(prefix).unwrap();

                    let value = match delimiter.filter(|delimiter| !delimiter.is_empty()) {
                        Some(delimiter) if suffix.contains(delimiter) => {
                            let (before, _) = suffix.split_once(delimiter).unwrap();
                            format!("{prefix}{before}{delimiter}")
                        }
                        _ => key.clone(),
                    };

                    if start_after.is_none_or(|after| value.as_str() > after) {
                        expected.insert(value);
                    }
                }

                for size in [1, 2, 3, 1000] {
                    let mut input = request();
                    input.prefix = prefix.map(str::to_owned);
                    input.delimiter = delimiter.map(str::to_owned);
                    input.start_after = start_after.map(str::to_owned);
                    input.max_keys = ObjectPageSize::new(size)?;
                    let mut actual = Vec::new();
                    let mut pages = 0;

                    loop {
                        let page = database.list_objects(&Authority::Native, &input)?;

                        assert!(
                            page.contents.len() + page.common_prefixes.len()
                                <= usize::try_from(size)?
                        );

                        actual.extend(page.contents.into_iter().map(|object| object.key));
                        actual.extend(page.common_prefixes);
                        pages += 1;
                        assert!(pages <= keys.len() + 1, "pagination did not advance");
                        input.continuation_token = page.next_continuation_token;

                        if input.continuation_token.is_none() {
                            break;
                        }
                    }

                    actual.sort();

                    assert_eq!(
                        actual,
                        expected.iter().cloned().collect::<Vec<_>>(),
                        "prefix={prefix:?} delimiter={delimiter:?} after={start_after:?} size={size}"
                    );
                }
            }
        }
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn tokens_reject_tampering_scope_and_other_operation_before_zero_page_success() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = seed(&directory, &["a".into(), "b".into()])?;
    let mut input = request();
    input.max_keys = ObjectPageSize::new(1)?;

    let token = database
        .list_objects(&Authority::Native, &input)?
        .next_continuation_token
        .unwrap();

    for invalid in [
        String::new(),
        "!".into(),
        token.get(..token.len() - 1).unwrap().into(),
        "A".repeat(100_000),
        format!("{token}A"),
    ] {
        input.continuation_token = Some(invalid);
        input.max_keys = ObjectPageSize::new(0)?;

        assert_eq!(
            database
                .list_objects(&Authority::Native, &input)
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidArgument
        );
    }

    input.continuation_token = Some(token.clone());
    input.prefix = Some(String::new());

    assert_eq!(
        database
            .list_objects(&Authority::Native, &input)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    input.prefix = None;
    input.delimiter = Some("/".into());

    assert_eq!(
        database
            .list_objects(&Authority::Native, &input)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    input.delimiter = None;

    assert_eq!(
        database
            .list_principals(crate::access::AdminPageSize::default(), Some(&token))
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    let zero = database.list_objects(&Authority::Native, &input)?;
    assert_eq!(zero.contents, []);
    assert_eq!(zero.common_prefixes, Vec::<String>::new());
    assert!(zero.next_continuation_token.is_none());
    input.continuation_token = None;
    input.expected_owner = Some("000000000000".into());

    assert_eq!(
        database
            .list_objects(&Authority::Native, &input)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn corrupt_references_and_lookahead_fail_instead_of_shortening_the_page() -> TestResult {
    for corruption in [
        "UPDATE blobs SET etag='invalid' WHERE blob_id=(SELECT blob_id FROM objects WHERE key=x'62')",
        "UPDATE blobs SET etag='\"not-a-digest\"' WHERE blob_id=(SELECT blob_id FROM objects WHERE key=x'62')",
        "UPDATE blobs SET md5_digest=zeroblob(16) WHERE blob_id=(SELECT blob_id FROM objects WHERE key=x'62')",
        "UPDATE blobs SET part_count=2 WHERE blob_id=(SELECT blob_id FROM objects WHERE key=x'62')",
        "UPDATE blobs SET shard=(shard+1)%256 WHERE blob_id=(SELECT blob_id FROM objects WHERE key=x'62')",
        "UPDATE blobs SET state='deleting', gc_pending=1 WHERE blob_id=(SELECT blob_id FROM objects WHERE key=x'62')",
        "UPDATE objects SET blob_id=zeroblob(16) WHERE key=x'62'",
        "UPDATE objects SET key=x'ff' WHERE key=x'62'",
    ] {
        let directory = TestDirectory::new()?;
        let mut database = seed(&directory, &["a".into(), "b".into()])?;

        database
            .connection
            .pragma_update(None, "foreign_keys", false)?;

        database
            .connection
            .pragma_update(None, "ignore_check_constraints", true)?;

        database.connection.execute(corruption, [])?;
        let mut input = request();
        input.max_keys = ObjectPageSize::new(1)?;

        let error = database
            .list_objects(&Authority::Native, &input)
            .expect_err(corruption);

        assert_eq!(
            error.failure(),
            OperationFailure::StoreCorrupt,
            "{corruption}"
        );

        close(database)?;
        directory.finish()?;
    }

    Ok(())
}

#[test]
fn thousand_entry_pages_and_long_lexical_filters_keep_exact_boundaries() -> TestResult {
    let directory = TestDirectory::new()?;

    let keys = (0..1001)
        .map(|index| format!("key-{index:04}"))
        .collect::<Vec<_>>();

    let mut database = seed(&directory, &keys)?;
    let mut input = request();
    let first = database.list_objects(&Authority::Native, &input)?;
    assert_eq!(first.contents.len(), 1000);
    assert_eq!(first.contents[999].key, "key-0999");
    input.continuation_token = first.next_continuation_token;
    assert!(input.continuation_token.is_some());
    let last = database.list_objects(&Authority::Native, &input)?;
    assert_eq!(last.contents.len(), 1);
    assert_eq!(last.contents[0].key, "key-1000");
    assert!(last.next_continuation_token.is_none());

    input.continuation_token = None;
    input.start_after = Some("key-0000".into());
    let exact = database.list_objects(&Authority::Native, &input)?;
    assert_eq!(exact.contents.len(), 1000);
    assert!(exact.next_continuation_token.is_none());
    input.start_after = Some("z".repeat(1025));

    assert_eq!(
        database.list_objects(&Authority::Native, &input)?.contents,
        []
    );

    input.start_after = None;
    input.prefix = Some("key".repeat(1025));

    assert_eq!(
        database.list_objects(&Authority::Native, &input)?.contents,
        []
    );

    input.prefix = None;
    input.delimiter = Some("/".repeat(1025));
    let ungrouped = database.list_objects(&Authority::Native, &input)?;
    assert_eq!(ungrouped.contents.len(), 1000);
    assert_eq!(ungrouped.common_prefixes, Vec::<String>::new());
    assert!(ungrouped.next_continuation_token.is_some());
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
#[allow(clippy::print_stderr, reason = "retained metadata-only measurement")]
fn dense_delimiter_subtree_is_skipped_and_exact_full_page_is_terminal() -> TestResult {
    let directory = TestDirectory::new()?;

    let mut keys: Vec<_> = (0..100_000)
        .map(|index| format!("dense/{index:06}"))
        .collect();

    keys.push("last".into());
    let mut database = seed(&directory, &keys)?;
    let mut input = request();
    input.delimiter = Some("/".into());
    input.max_keys = ObjectPageSize::new(1)?;
    let started = Instant::now();
    let first = database.list_objects(&Authority::Native, &input)?;
    assert_eq!(first.common_prefixes, ["dense/"]);
    assert_eq!(first.contents, []);
    input.continuation_token = first.next_continuation_token;
    assert!(input.continuation_token.is_some());
    let last = database.list_objects(&Authority::Native, &input)?;
    assert_eq!(last.contents[0].key, "last");
    assert!(last.next_continuation_token.is_none());
    let (steps, _) = crate::metadata::listing::indexed_work(&database.connection)?;
    assert!(steps > 0 && steps < 1000, "unexpected SQL work: {steps}");

    // The observation is explicitly metadata-only, without 100,000 payloads.
    eprintln!(
        "LISTING_MEASUREMENT rows=100001 pages=2 vm_steps={steps} elapsed_us={}",
        started.elapsed().as_micros()
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn flat_delimiter_pages_batch_rows_without_per_object_queries() -> TestResult {
    let directory = TestDirectory::new()?;
    let keys: Vec<_> = (0..65).map(|index| format!("key-{index:04}")).collect();
    let mut database = seed(&directory, &keys)?;

    for prefix in [None, Some("key-".to_owned())] {
        let mut input = request();
        input.prefix = prefix;
        input.delimiter = Some("/".into());
        input.max_keys = ObjectPageSize::new(64)?;
        let (_, before) = crate::metadata::listing::indexed_work(&database.connection)?;
        let first = database.list_objects(&Authority::Native, &input)?;
        let (_, after) = crate::metadata::listing::indexed_work(&database.connection)?;

        assert_eq!(first.contents.len(), 64);
        assert_eq!(first.contents[0].key, keys[0]);
        assert_eq!(first.contents[63].key, keys[63]);
        assert_eq!(first.common_prefixes, Vec::<String>::new());
        assert!(first.next_continuation_token.is_some());

        assert!(
            after - before <= 2,
            "per-object SQL queries: {}",
            after - before
        );

        input.continuation_token = first.next_continuation_token;
        let last = database.list_objects(&Authority::Native, &input)?;
        assert_eq!(last.contents.len(), 1);
        assert_eq!(last.contents[0].key, keys[64]);
        assert!(last.next_continuation_token.is_none());
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}
