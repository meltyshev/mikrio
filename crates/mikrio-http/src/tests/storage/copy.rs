use super::*;

#[test]
fn authenticated_copy_publishes_bytes_and_checks_independent_source_and_destination_assertions()
-> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/source%2B%252f",
                "abc",
                &[("x-amz-meta-origin", "source")],
            ))?,
            200,
        );

        let copy = fixture.exchange(&request(
            "PUT",
            "/photos/copy",
            "",
            &[
                ("x-amz-copy-source", "/photos/source%2B%252f"),
                (
                    "x-amz-copy-source-if-match",
                    "\"900150983cd24fb0d6963f7d28e17f72\"",
                ),
                ("if-none-match", "*"),
                ("x-amz-checksum-algorithm", "SHA256"),
            ],
        ))?;

        status(&copy, 200);
        assert!(copy.contains("<CopyObjectResult>"), "{copy}");

        assert!(
            copy.contains(
                "<ChecksumSHA256>ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=</ChecksumSHA256>"
            ),
            "{copy}"
        );

        assert_eq!(bytes(fixture, "copy"), b"abc");

        assert_eq!(
            fixture
                .runtime
                .block_on(fixture.connection.head_object(read("copy")))?
                .metadata
                .entries()["origin"],
            "source"
        );

        for (name, value, expected) in [
            ("if-none-match", "*", 200),
            ("x-amz-copy-source-if-match", "\"different\"", 412),
            ("x-amz-source-expected-bucket-owner", "000000000000", 403),
        ] {
            report_case(format_args!(
                "name={name:?}, value={value:?}, expected={expected:?}"
            ));

            let response = fixture.exchange(&request(
                "PUT",
                "/photos/copy",
                "",
                &[
                    ("x-amz-copy-source", "/photos/source%2B%252f"),
                    (name, value),
                ],
            ))?;

            status(&response, expected);

            if name == "if-none-match" {
                assert!(
                    response.contains("<Code>PreconditionFailed</Code>"),
                    "{response}"
                );
            }

            assert_eq!(bytes(fixture, "copy"), b"abc");
        }

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/copy",
                "unexpected",
                &[("x-amz-copy-source", "/photos/source%2B%252f")],
            ))?,
            400,
        );

        assert_eq!(bytes(fixture, "copy"), b"abc");
        Ok(())
    })
}
