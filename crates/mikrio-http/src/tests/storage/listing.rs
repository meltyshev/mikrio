use super::*;

#[test]
fn bucket_and_object_listing_preserve_the_shared_timestamp_range() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/dated", "abc", &[]))?,
            200,
        );

        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":["s3:ListBucket","s3:ListAllMyBuckets"],"Resource":"*"}}"#],
        )?;

        for (milliseconds, expected) in [
            (-62_135_596_800_000_i64, "0001-01-01T00:00:00.000Z"),
            (-1, "1969-12-31T23:59:59.999Z"),
            (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
        ] {
            // Persisted instants need not fit the host's SystemTime representation.
            // Both list routes must preserve the same range as other HTTP outputs.
            fixture.database.execute(
                "UPDATE buckets SET created_at_ms=?1 WHERE name='photos'",
                [milliseconds],
            )?;

            fixture
                .database
                .execute("UPDATE objects SET last_modified_at_ms=?1", [milliseconds])?;

            for (path, field) in [
                ("/photos?list-type=2", "LastModified"),
                ("/", "CreationDate"),
                ("/?max-buckets=1", "CreationDate"),
            ] {
                report_case(format_args!(
                    "milliseconds={milliseconds:?}, expected={expected:?}, path={path:?}, field={field:?}"
                ));

                let response = fixture.exchange(&request("GET", path, "", &[]))?;
                status(&response, 200);

                assert!(
                    response.contains(&format!("<{field}>{expected}</{field}>")),
                    "{path} did not preserve {milliseconds}: {response}"
                );
            }
        }

        Ok(())
    })
}

#[test]
fn signed_listing_projects_grouped_encoded_pages_and_rejects_invalid_options() -> TestResult {
    with_fixture(|fixture| {
        for key in ["a", "b/one", "b/two", "c+雪"] {
            report_case(format_args!("key={key:?}"));

            status(
                &fixture.exchange(&request(
                    "PUT",
                    &format!("/photos/{}", urlencoding::encode(key)),
                    "abc",
                    &[],
                ))?,
                200,
            );
        }

        let first = fixture.exchange(&request(
            "GET",
            "/photos?list-type=2&delimiter=%2F&max-keys=2&encoding-type=url",
            "",
            &[],
        ))?;

        status(&first, 200);
        assert!(first.contains("<KeyCount>2</KeyCount>"));
        assert!(first.contains("<Key>a</Key>"));
        assert!(first.contains("<Prefix>b%2F</Prefix>"));
        assert!(first.contains("<IsTruncated>true</IsTruncated>"));

        let token = first
            .split_once("<NextContinuationToken>")
            .unwrap()
            .1
            .split_once("</NextContinuationToken>")
            .unwrap()
            .0;

        let second = fixture.exchange(&request("GET", &format!("/photos?list-type=2&delimiter=%2F&max-keys=1&encoding-type=url&continuation-token={token}"), "", &[]))?;
        status(&second, 200);
        assert!(second.contains("<Key>c%2B%E9%9B%AA</Key>"));
        assert!(second.contains("<IsTruncated>false</IsTruncated>"));
        assert!(!second.contains("<NextContinuationToken>"));

        let empty = fixture.exchange(&request("GET", "/photos?list-type=2&max-keys=0", "", &[]))?;
        status(&empty, 200);
        assert!(empty.contains("<KeyCount>0</KeyCount>"));
        assert!(!empty.contains("<Contents>"));
        assert!(!empty.contains("<NextContinuationToken>"));

        for query in [
            "max-keys=-1",
            "max-keys=1001",
            "max-keys=2147483648",
            "encoding-type=bogus",
            "continuation-token=",
        ] {
            let response = fixture.exchange(&request(
                "GET",
                &format!("/photos?list-type=2&{query}"),
                "",
                &[],
            ))?;

            assert!(response.starts_with("HTTP/1.1 400 "), "{query}: {response}");
        }

        status(
            &fixture.exchange(&request(
                "GET",
                "/photos?list-type=2&fetch-owner=true",
                "",
                &[],
            ))?,
            501,
        );

        for payer in ["", "requester"] {
            report_case(format_args!("payer={payer:?}"));

            status(
                &fixture.exchange(&request(
                    "GET",
                    "/photos?list-type=2",
                    "",
                    &[("x-amz-request-payer", payer)],
                ))?,
                501,
            );
        }

        status(
            &fixture.exchange(&request("GET", "/photos?list-type=2", "unexpected", &[]))?,
            400,
        );

        Ok(())
    })
}

#[test]
fn prefix_permission_applies_to_each_page_and_a_token_never_grants_access() -> TestResult {
    with_fixture(|fixture| {
        for key in ["owned/a", "owned/b", "outside/private"] {
            report_case(format_args!("key={key:?}"));

            status(
                &fixture.exchange(&request("PUT", &format!("/photos/{key}"), "abc", &[]))?,
                200,
            );
        }

        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:ListBucket","Resource":"arn:aws:s3:::photos","Condition":{"StringEquals":{"s3:prefix":"owned/"}}}}"#,
        ])?;

        let first = fixture.exchange(&request(
            "GET",
            "/photos?list-type=2&prefix=owned%2F&max-keys=1",
            "",
            &[],
        ))?;

        status(&first, 200);

        let token = first
            .split_once("<NextContinuationToken>")
            .unwrap()
            .1
            .split_once("</NextContinuationToken>")
            .unwrap()
            .0;

        for prefix in ["", "&prefix=", "&prefix=owned", "&prefix=outside%2F"] {
            report_case(format_args!("prefix={prefix:?}"));

            status(
                &fixture.exchange(&request(
                    "GET",
                    &format!("/photos?list-type=2{prefix}&continuation-token={token}"),
                    "",
                    &[],
                ))?,
                403,
            );
        }

        fixture
            .database
            .execute("UPDATE principals SET is_enabled=0", [])?;

        status(
            &fixture.exchange(&request(
                "GET",
                &format!("/photos?list-type=2&prefix=owned%2F&continuation-token={token}"),
                "",
                &[],
            ))?,
            403,
        );

        // Trusted native authority stays independent of the HTTP credential.
        let page = fixture
            .runtime
            .block_on(fixture.connection.list_objects_v2(
                mikrio_core::object::ListObjectsRequest {
                    bucket: BucketName::parse("photos")?,
                    expected_owner: None,
                    prefix: None,
                    delimiter: None,
                    start_after: None,
                    continuation_token: None,
                    max_keys: mikrio_core::object::ObjectPageSize::default(),
                },
            ))?;

        assert_eq!(page.contents.len(), 3);
        Ok(())
    })
}
