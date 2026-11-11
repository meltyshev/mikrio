use super::*;

const LIFECYCLE: &str = "<LifecycleConfiguration><Rule><ID>保留-😀</ID><Filter><Prefix>drafts/</Prefix></Filter><Status>Enabled</Status><Expiration><Days>7</Days></Expiration><AbortIncompleteMultipartUpload><DaysAfterInitiation>1</DaysAfterInitiation></AbortIncompleteMultipartUpload></Rule><Rule><Filter/><Status>Disabled</Status><Expiration><Days>2147483647</Days></Expiration></Rule><Rule><Prefix></Prefix><Status>Disabled</Status><AbortIncompleteMultipartUpload><DaysAfterInitiation>2</DaysAfterInitiation></AbortIncompleteMultipartUpload></Rule></LifecycleConfiguration>";

fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").unwrap().1
}

#[test]
fn lifecycle_http_round_trip_preserves_filters_generated_ids_and_authority() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        let missing = fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?;
        status(&missing, 404);
        assert!(missing.contains("<Code>NoSuchLifecycleConfiguration</Code>"));

        status(
            &fixture.exchange(&request("PUT", "/photos?lifecycle", LIFECYCLE, &[]))?,
            200,
        );

        let original = fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?;
        status(&original, 200);
        assert!(body(&original).contains("<DaysAfterInitiation>1</DaysAfterInitiation>"));
        assert!(body(&original).contains("<Prefix></Prefix>"));

        let stored = fixture
            .runtime
            .block_on(fixture.connection.get_bucket_lifecycle(bucket()))?
            .unwrap();

        assert_eq!(stored.rules().len(), 3);

        assert!(
            stored
                .rules()
                .iter()
                .all(|rule| rule.id.as_ref().is_some_and(|id| !id.is_empty()))
        );

        assert_eq!(
            stored.rules()[1].filter,
            mikrio_core::lifecycle::LifecycleFilter::All
        );

        assert_eq!(
            stored.rules()[2].filter,
            mikrio_core::lifecycle::LifecycleFilter::LegacyPrefix(String::new())
        );

        status(
            &fixture.exchange(&request("PUT", "/photos?lifecycle", body(&original), &[]))?,
            200,
        );

        assert_eq!(
            body(&fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?),
            body(&original)
        );

        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetLifecycleConfiguration","Resource":"*"}}"#])?;

        status(
            &fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?,
            200,
        );

        for (method, payload) in [("PUT", LIFECYCLE), ("DELETE", "")] {
            report_case(format_args!("method={method:?}"));

            status(
                &fixture.exchange(&request(method, "/photos?lifecycle", payload, &[]))?,
                403,
            );
        }

        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        for attempt in 0..2 {
            report_case(format_args!("delete_attempt={attempt}"));

            status(
                &fixture.exchange(&request("DELETE", "/photos?lifecycle", "", &[]))?,
                204,
            );
        }

        status(
            &fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?,
            404,
        );

        Ok(())
    })
}

#[test]
fn lifecycle_http_invalid_later_rules_checksum_and_framing_preserve_predecessor() -> TestResult {
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos?lifecycle", LIFECYCLE, &[]))?,
            200,
        );

        let original = fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?;

        for (name, value, code) in [
            ("content-md5", "kAFQmDzST7DWlj99KOF/cg==", "BadDigest"),
            ("x-amz-checksum-crc32", "NSRBwg==", "BadDigest"),
            ("content-md5", "", "InvalidDigest"),
        ] {
            report_case(format_args!(
                "name={name:?}, value={value:?}, code={code:?}"
            ));

            let response =
                fixture.exchange(&request("GET", "/photos?lifecycle", "", &[(name, value)]))?;

            status(&response, 400);

            assert!(
                response.contains(&format!("<Code>{code}</Code>")),
                "{response}"
            );
        }

        for (invalid, expected) in [
            (format!("{LIFECYCLE}tail"), 400),
            (LIFECYCLE.replace("<Days>2147483647", "<Days>0"), 400),
            (LIFECYCLE.replace("<Days>7", "<Days>2147483648"), 400),
            (LIFECYCLE.replace("<Days>7", "<Days>1tail"), 400),
            (
                LIFECYCLE.replace(
                    "<Filter/>",
                    "<Filter><Prefix>x</Prefix><Prefix>y</Prefix></Filter>",
                ),
                400,
            ),
            (
                LIFECYCLE.replace("<Filter/>", "<Filter/><Prefix>x</Prefix>"),
                400,
            ),
            (
                LIFECYCLE.replace("<Filter/>", "<Filter><Unknown/></Filter>"),
                400,
            ),
            (
                LIFECYCLE.replace(
                    "<Filter/>",
                    "<Filter><Tag><Key>x</Key><Value>y</Value></Tag></Filter>",
                ),
                501,
            ),
            (
                LIFECYCLE.replace("<Days>7</Days>", "<Date>2026-12-31T00:00:00Z</Date>"),
                501,
            ),
            (
                LIFECYCLE.replace(
                    "<Days>7</Days>",
                    "<ExpiredObjectDeleteMarker>false</ExpiredObjectDeleteMarker>",
                ),
                501,
            ),
            (
                LIFECYCLE.replace("<Filter/>", "<ID>保留-😀</ID><Filter/>"),
                400,
            ),
        ] {
            report_case(format_args!("invalid={invalid:?}, expected={expected:?}"));

            let response = fixture.exchange(&request("PUT", "/photos?lifecycle", &invalid, &[]))?;
            status(&response, expected);

            assert_eq!(
                body(&fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?),
                body(&original)
            );
        }

        for (name, value, expected) in [
            ("content-md5", "1B2M2Y8AsgTpgAmY7PhCfg==", 400),
            ("x-amz-expected-bucket-owner", "", 400),
            ("x-amz-transition-default-minimum-object-size", "", 501),
        ] {
            report_case(format_args!(
                "name={name:?}, value={value:?}, expected={expected:?}"
            ));

            status(
                &fixture.exchange(&request(
                    "PUT",
                    "/photos?lifecycle",
                    LIFECYCLE,
                    &[(name, value)],
                ))?,
                expected,
            );
        }

        status(
            &fixture.exchange(&request("DELETE", "/photos?lifecycle", "unexpected", &[]))?,
            400,
        );

        assert_eq!(
            body(&fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?),
            body(&original)
        );

        Ok(())
    })
}

#[test]
fn lifecycle_unsupported_actions_reject_valid_enabled_and_disabled_rules_atomically() -> TestResult
{
    with_fixture(|fixture| {
        fixture.database.execute(
            "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
            [ADMIN],
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos?lifecycle", LIFECYCLE, &[]))?,
            200,
        );

        let before = fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?;

        for enabled in ["Enabled", "Disabled"] {
            for action in [
                "<Transition><Days>30</Days><StorageClass>GLACIER</StorageClass></Transition>",
                "<NoncurrentVersionTransition><NoncurrentDays>30</NoncurrentDays><StorageClass>STANDARD_IA</StorageClass></NoncurrentVersionTransition>",
                "<NoncurrentVersionExpiration><NoncurrentDays>7</NoncurrentDays></NoncurrentVersionExpiration>",
            ] {
                report_case(format_args!("status={enabled}, action={action}"));

                let configuration = format!(
                    "<LifecycleConfiguration><Rule><ID>supported-first</ID><Filter/><Status>Enabled</Status><Expiration><Days>5</Days></Expiration></Rule><Rule><ID>unsupported-later</ID><Filter/><Status>{enabled}</Status>{action}</Rule></LifecycleConfiguration>"
                );

                let response =
                    fixture.exchange(&request("PUT", "/photos?lifecycle", &configuration, &[]))?;

                status(&response, 501);
                assert!(response.contains("<Code>NotImplemented</Code>"));

                let after = fixture.exchange(&request("GET", "/photos?lifecycle", "", &[]))?;
                status(&after, 200);
                assert_eq!(body(&after), body(&before));
            }
        }

        Ok(())
    })
}
