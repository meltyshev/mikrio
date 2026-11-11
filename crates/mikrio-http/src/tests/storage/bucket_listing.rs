use super::*;

fn allow_listing(fixture: &Fixture<'_>) -> TestResult {
    fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:ListAllMyBuckets","Resource":"*"}}"#,
    ])?;

    Ok(())
}

#[test]
fn bucket_listing_rechecks_account_permission_and_tokens_never_authorize() -> TestResult {
    with_fixture(|fixture| {
        fixture
            .runtime
            .block_on(fixture.connection.create_bucket(BucketName::parse("aaa")?))?;

        status(&fixture.exchange(&request("GET", "/", "", &[]))?, 403);
        allow_listing(fixture)?;

        // Per-bucket denial cannot change this account-wide operation into a filter.
        fixture.database.execute("UPDATE buckets SET policy_generation=randomblob(16), policy_json=?1 WHERE name='photos'", [
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:ListBucket","Resource":"arn:aws:s3:::photos"}}"#,
        ])?;

        let first = fixture.exchange(&request("GET", "/?max-buckets=1", "", &[]))?;
        status(&first, 200);
        assert!(first.contains("<Name>aaa</Name>"));
        assert!(!first.contains("<IsTruncated>"));
        assert!(!first.contains("<DisplayName>"));

        let token = first
            .split_once("<ContinuationToken>")
            .unwrap()
            .1
            .split_once("</ContinuationToken>")
            .unwrap()
            .0;

        let path = format!("/?max-buckets=1&continuation-token={token}");
        let last = fixture.exchange(&request("GET", &path, "", &[]))?;
        status(&last, 200);
        assert!(last.contains("<Name>photos</Name>"));
        assert!(!last.contains("<ContinuationToken>"));

        for query in [
            format!("prefix=&continuation-token={token}"),
            format!("bucket-region=us-east-1&continuation-token={token}"),
            "continuation-token=".into(),
        ] {
            report_case(format_args!("query={query:?}"));

            status(
                &fixture.exchange(&request("GET", &format!("/?{query}"), "", &[]))?,
                400,
            );
        }

        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:ListAllMyBuckets","Resource":"*"},{"Effect":"Deny","Action":"s3:ListAllMyBuckets","Resource":"*"}]}"#,
        ])?;

        status(&fixture.exchange(&request("GET", &path, "", &[]))?, 403);
        allow_listing(fixture)?;

        fixture
            .database
            .execute("UPDATE principals SET is_enabled=0", [])?;

        status(&fixture.exchange(&request("GET", &path, "", &[]))?, 403);
        status(&fixture.exchange(b"GET / HTTP/1.1\r\nhost: localhost:9000\r\nconnection: close\r\nx-mikrio-authority: native\r\n\r\n")?, 403);

        let page = fixture.runtime.block_on(fixture.connection.list_buckets(
            mikrio_core::object::ListBucketsRequest {
                prefix: None,
                continuation_token: None,
                max_buckets: Some(mikrio_core::object::BucketPageSize::default()),
                bucket_region: None,
            },
        ))?;

        assert_eq!(page.buckets.len(), 2);
        Ok(())
    })
}

#[test]
fn bucket_listing_enforces_wire_options_and_complete_empty_envelopes() -> TestResult {
    with_fixture(|fixture| {
        allow_listing(fixture)?;

        for query in [
            "max-buckets=0",
            "max-buckets=-1",
            "max-buckets=10001",
            "max-buckets=1tail",
            "bucket-region=us-west-2",
            "bucket-region=",
        ] {
            report_case(format_args!("query={query:?}"));

            status(
                &fixture.exchange(&request("GET", &format!("/?{query}"), "", &[]))?,
                400,
            );
        }

        let filtered = fixture.exchange(&request(
            "GET",
            "/?prefix=ph&bucket-region=us-east-1",
            "",
            &[],
        ))?;

        status(&filtered, 200);
        assert!(filtered.contains("<Prefix>ph</Prefix>"));
        assert!(filtered.contains("<BucketRegion>us-east-1</BucketRegion>"));

        for prefix in ["PH", "%E9%9B%AA", "..%2F", "p%25"] {
            report_case(format_args!("prefix={prefix:?}"));

            let empty =
                fixture.exchange(&request("GET", &format!("/?prefix={prefix}"), "", &[]))?;

            status(&empty, 200);
            assert!(!empty.contains("<Bucket>"));
            assert!(!empty.contains("<ContinuationToken>"));
        }

        for header in ["x-amz-expected-bucket-owner", "x-amz-request-payer"] {
            report_case(format_args!("header={header:?}"));

            status(
                &fixture.exchange(&request("GET", "/", "", &[(header, "000000000000")]))?,
                501,
            );
        }

        status(
            &fixture.exchange(&request("GET", "/", "unexpected", &[]))?,
            400,
        );

        Ok(())
    })
}

#[test]
#[allow(
    clippy::print_stderr,
    reason = "retained full-list HTTP serialization measurement"
)]
fn unpaginated_http_buckets_return_the_complete_snapshot_without_a_local_quota() -> TestResult {
    with_fixture(|fixture| {
        allow_listing(fixture)?;
        // Metadata-only fixture: this measures the production selector and wire
        // serializer, not 100,000 public creation transactions or object payloads.
        let transaction = fixture.database.unchecked_transaction()?;

        {
            let mut insert = transaction.prepare(
                "INSERT INTO buckets (bucket_id, name, created_at_ms) VALUES (?1, ?2, 0)",
            )?;

            for index in 0..100_000_u128 {
                insert.execute(rusqlite::params![
                    index.to_be_bytes(),
                    format!("bucket-{index:06}")
                ])?;
            }
        }

        transaction.commit()?;
        let started = std::time::Instant::now();
        let all = fixture.exchange(&request("GET", "/", "", &[]))?;
        status(&all, 200);
        assert_eq!(all.matches("<Bucket>").count(), 100_001);
        assert!(!all.contains("<ContinuationToken>"));
        assert!(!all.contains("<BucketRegion>"));
        assert!(!all.contains("<Prefix>"));

        eprintln!(
            "BUCKET_HTTP_MEASUREMENT live=100001 response_bytes={} elapsed_us={}",
            all.len(),
            started.elapsed().as_micros()
        );

        let paged = fixture.exchange(&request("GET", "/?prefix=", "", &[]))?;
        status(&paged, 200);
        assert_eq!(paged.matches("<Bucket>").count(), 10_000);
        assert!(paged.contains("<ContinuationToken>"));
        assert!(paged.contains("<BucketRegion>us-east-1</BucketRegion>"));
        Ok(())
    })
}
