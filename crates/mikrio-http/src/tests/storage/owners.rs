//! Expected-owner syntax is validated before storage admission, then compared.

use super::*;

fn reservation_sequence(fixture: &Fixture<'_>) -> rusqlite::Result<i64> {
    fixture.database.query_row(
        "SELECT seq FROM sqlite_sequence WHERE name = 'blobs'",
        [],
        |row| row.get(0),
    )
}

#[test]
fn malformed_owner_assertions_fail_before_namespace_lookup_or_reservation() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "predecessor", &[]))?,
            200,
        );

        let sequence = reservation_sequence(fixture)?;

        for owner in ["", "12345678901", "1234567890123", "12345678901x"] {
            for method in ["PUT", "GET", "HEAD", "DELETE"] {
                for path in ["/photos/object", "/photos/missing", "/absent/object"] {
                    report_case(format_args!(
                        "owner={owner:?}, method={method:?}, path={path:?}"
                    ));

                    let response = fixture.exchange(&request(
                        method,
                        path,
                        if method == "PUT" { "replacement" } else { "" },
                        &[("x-amz-expected-bucket-owner", owner)],
                    ))?;

                    status(&response, 400);

                    if method == "HEAD" {
                        assert!(response.ends_with("\r\n\r\n"));
                    } else {
                        assert!(response.contains("<Code>InvalidArgument</Code>"));
                    }

                    assert_eq!(reservation_sequence(fixture)?, sequence);
                    assert_eq!(bytes(fixture, "object"), b"predecessor");
                }
            }
        }

        Ok(())
    })
}

#[test]
fn valid_owner_assertions_preserve_matching_success_and_mismatching_denial() -> TestResult {
    with_fixture(|fixture| {
        let owner: String = fixture.database.query_row(
            "SELECT owner_account_id FROM store_metadata WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )?;

        let mismatch = if owner == "000000000000" {
            "000000000001"
        } else {
            "000000000000"
        };

        status(
            &fixture.exchange(&request(
                "PUT",
                "/photos/object",
                "predecessor",
                &[("x-amz-expected-bucket-owner", &owner)],
            ))?,
            200,
        );

        let sequence = reservation_sequence(fixture)?;

        for method in ["PUT", "GET", "HEAD", "DELETE"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos/object",
                if method == "PUT" { "replacement" } else { "" },
                &[("x-amz-expected-bucket-owner", mismatch)],
            ))?;

            status(&response, 403);

            if method == "HEAD" {
                assert!(response.ends_with("\r\n\r\n"));
            } else {
                assert!(response.contains("<Code>AccessDenied</Code>"));
            }

            assert_eq!(reservation_sequence(fixture)?, sequence);
            assert_eq!(bytes(fixture, "object"), b"predecessor");
        }

        for (method, expected) in [("GET", 200), ("HEAD", 200), ("DELETE", 204)] {
            report_case(format_args!("method={method:?}, expected={expected:?}"));

            status(
                &fixture.exchange(&request(
                    method,
                    "/photos/object",
                    "",
                    &[("x-amz-expected-bucket-owner", &owner)],
                ))?,
                expected,
            );
        }

        Ok(())
    })
}
