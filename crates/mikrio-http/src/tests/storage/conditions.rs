//! Literal expected outcomes for every absent/matching/failing condition tuple.

use super::*;

#[test]
fn all_read_condition_combinations_preserve_precedence_before_range_selection() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "testcontent", &[]))?,
            200,
        );

        let selected = fixture
            .runtime
            .block_on(fixture.connection.head_object(read("object")))?;

        let tags = [None, Some(selected.etag.as_str()), Some("\"different\"")];

        // Rows: If-Match absent/matching/failing * If-None-Match absent/matching/failing.
        // Columns: If-Modified-Since absent/past/future * If-Unmodified-Since absent/past/future.
        // AWS's documented tag/date pairs and RFC 9110 section 13.2.2 determine these
        // literal results; neither the core evaluator nor a reference server generates them.
        let expected = [
            [200, 412, 200, 200, 412, 200, 304, 412, 304],
            [304, 412, 304, 304, 412, 304, 304, 412, 304],
            [200, 412, 200, 200, 412, 200, 200, 412, 200],
            [200, 200, 200, 200, 200, 200, 304, 304, 304],
            [304; 9],
            [200; 9],
            [412; 9],
            [412; 9],
            [412; 9],
        ];

        for (row, outcomes) in expected.into_iter().enumerate() {
            check_dates(fixture, tags[row / 3], tags[row % 3], outcomes)?;
        }

        assert_eq!(bytes(fixture, "object"), b"testcontent");
        Ok(())
    })
}

fn check_dates(
    fixture: &Fixture<'_>,
    if_match: Option<&str>,
    if_none_match: Option<&str>,
    outcomes: [u16; 9],
) -> TestResult {
    let dates = [
        None,
        Some("Thu, 01 Jan 1970 00:00:00 GMT"),
        Some("Fri, 31 Dec 9999 23:59:59 GMT"),
    ];

    for (column, expected) in outcomes.into_iter().enumerate() {
        let headers: Vec<_> = [
            ("if-match", if_match),
            ("if-none-match", if_none_match),
            ("if-modified-since", dates[column / 3]),
            ("if-unmodified-since", dates[column % 3]),
            ("range", Some("bytes=40-50")),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect();

        for method in ["GET", "HEAD"] {
            report_case(format_args!(
                "column={column:?}, expected={expected:?}, method={method:?}"
            ));

            let response = fixture.exchange(&request(method, "/photos/object", "", &headers))?;

            // A passing condition tuple reaches the unsatisfiable range. A
            // failed tuple must keep its own status and never produce a body handle.
            let expected = if expected == 200 { 416 } else { expected };

            assert!(
                response.starts_with(&format!("HTTP/1.1 {expected} ")),
                "{method} {headers:?}: {response}"
            );

            let body = response.split_once("\r\n\r\n").unwrap().1;

            if method == "HEAD" || expected == 304 {
                assert_eq!(body, "", "{method} {headers:?}");
                continue;
            }

            let code = if expected == 416 {
                "InvalidRange"
            } else {
                "PreconditionFailed"
            };

            assert!(body.contains(&format!("<Code>{code}</Code>")));
        }
    }

    Ok(())
}
