use serde_json::json;

use super::*;

fn policy(fixture: &Fixture<'_>, statements: &serde_json::Value) -> TestResult {
    fixture.database.execute(
        "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
        [json!({"Version":"2012-10-17", "Statement":statements}).to_string()],
    )?;

    Ok(())
}

#[test]
fn every_attribute_subset_projects_only_selected_groups_over_signed_http() -> TestResult {
    with_fixture(|fixture| {
        policy(
            fixture,
            &json!({"Effect":"Allow", "Action":["s3:GetObject", "s3:GetObjectAttributes", "s3:PutObject"], "Resource":"*"}),
        )?;

        status(
            &fixture.exchange(&request("PUT", "/photos/ordinary", "abc", &[]))?,
            200,
        );

        seed(fixture, "composite", ChecksumType::Composite)?;

        let groups = [
            "ETag",
            "Checksum",
            "ObjectParts",
            "StorageClass",
            "ObjectSize",
        ];

        for key in ["ordinary", "composite"] {
            for mask in 1..32 {
                report_case(format_args!("key={key:?}, mask={mask:?}"));

                let selectors = groups
                    .iter()
                    .enumerate()
                    .filter_map(|(index, group)| (mask & (1 << index) != 0).then_some(*group))
                    .collect::<Vec<_>>()
                    .join(", ");

                let response = fixture.exchange(&request(
                    "GET",
                    &format!("/photos/{key}?attributes"),
                    "",
                    &[("x-amz-object-attributes", &selectors)],
                ))?;

                status(&response, 200);
                assert!(response.contains("last-modified: "));

                for (index, group) in groups.iter().enumerate() {
                    let present =
                        mask & (1 << index) != 0 && (*group != "ObjectParts" || key == "composite");

                    assert_eq!(
                        response.contains(&format!("<{group}>")),
                        present,
                        "{key}/{mask}/{group}: {response}"
                    );
                }
            }
        }

        Ok(())
    })
}

#[test]
fn attributes_http_requires_both_current_grants_and_preserves_missing_disclosure() -> TestResult {
    with_fixture(|fixture| {
        status(
            &fixture.exchange(&request("PUT", "/photos/object", "abc", &[]))?,
            200,
        );

        let headers = [("x-amz-object-attributes", "ETag,ObjectSize")];

        for (grants, denied, expected) in [
            (vec!["s3:ListBucket"], None, 403),
            (vec!["s3:GetObject"], None, 403),
            (vec!["s3:GetObjectAttributes"], None, 403),
            (vec!["s3:GetObject", "s3:GetObjectAttributes"], None, 200),
            (
                vec!["s3:GetObject", "s3:GetObjectAttributes"],
                Some("s3:GetObject"),
                403,
            ),
            (
                vec!["s3:GetObject", "s3:GetObjectAttributes"],
                Some("s3:GetObjectAttributes"),
                403,
            ),
        ] {
            report_case(format_args!(
                "grants={grants:?}, denied={denied:?}, expected={expected:?}"
            ));

            let mut statements = vec![json!({"Effect":"Allow", "Action":grants, "Resource":"*"})];

            if let Some(action) = denied {
                statements.push(json!({"Effect":"Deny", "Action":action, "Resource":"*"}));
            }

            policy(fixture, &json!(statements))?;

            let response =
                fixture.exchange(&request("GET", "/photos/object?attributes", "", &headers))?;

            status(&response, expected);

            assert_eq!(
                response.contains("<ObjectSize>3</ObjectSize>"),
                expected == 200
            );
        }

        for can_list in [false, true] {
            report_case(format_args!("can_list={can_list:?}"));

            let mut actions = vec!["s3:GetObject", "s3:GetObjectAttributes"];

            if can_list {
                actions.push("s3:ListBucket");
            }

            policy(
                fixture,
                &json!({"Effect":"Allow", "Action":actions, "Resource":"*"}),
            )?;

            status(
                &fixture.exchange(&request("GET", "/photos/missing?attributes", "", &headers))?,
                if can_list { 404 } else { 403 },
            );
        }

        policy(
            fixture,
            &json!({"Effect":"Allow", "Action":["s3:GetObject", "s3:GetObjectAttributes"], "Resource":"*",
            "Condition":{"StringEquals":{"s3:ExistingObjectTag/team":"allowed"}}}),
        )?;

        fixture
            .runtime
            .block_on(fixture.connection.replace_object_tags(
                read("object"),
                mikrio_core::object::ObjectTags::new([("team".into(), "allowed".into())])?,
            ))?;

        status(
            &fixture.exchange(&request("GET", "/photos/object?attributes", "", &headers))?,
            200,
        );

        fixture.runtime.block_on(
            fixture
                .connection
                .replace_object_tags(read("object"), mikrio_core::object::ObjectTags::default()),
        )?;

        status(
            &fixture.exchange(&request("GET", "/photos/object?attributes", "", &headers))?,
            403,
        );

        policy(
            fixture,
            &json!({"Effect":"Allow", "Action":["s3:GetObject", "s3:GetObjectAttributes"], "Resource":"*"}),
        )?;

        let owner: String = fixture.database.query_row(
            "SELECT owner_account_id FROM store_metadata",
            [],
            |row| row.get(0),
        )?;

        let mismatch = if owner == "000000000000" {
            "000000000001"
        } else {
            "000000000000"
        };

        for (owner, expected) in [(owner.as_str(), 200), (mismatch, 403), ("", 400)] {
            report_case(format_args!("owner={owner:?}, expected={expected:?}"));

            status(
                &fixture.exchange(&request(
                    "GET",
                    "/photos/object?attributes",
                    "",
                    &[
                        ("x-amz-object-attributes", "ETag"),
                        ("x-amz-expected-bucket-owner", owner),
                    ],
                ))?,
                expected,
            );
        }

        let anonymous = b"GET /photos/object?attributes HTTP/1.1\r\nhost: localhost\r\nx-amz-object-attributes: ETag,ObjectSize\r\nconnection: close\r\n\r\n";

        for (actions, expected) in [
            (vec!["s3:GetObject"], 403),
            (vec!["s3:GetObject", "s3:GetObjectAttributes"], 200),
        ] {
            report_case(format_args!("actions={actions:?}, expected={expected:?}"));

            let document = json!({"Version":"2012-10-17", "Statement":{
                "Effect":"Allow", "Principal":"*", "Action":actions,
                "Resource":"arn:aws:s3:::photos/*"
            }});

            fixture.database.execute(
                "UPDATE buckets SET block_public_acls=0, ignore_public_acls=0,
                 block_public_policy=0, restrict_public_buckets=0,
                 policy_generation=randomblob(16), policy_json=?1",
                [document.to_string()],
            )?;

            status(&fixture.exchange(anonymous)?, expected);
        }

        fixture
            .database
            .execute("UPDATE buckets SET restrict_public_buckets=1", [])?;

        status(&fixture.exchange(anonymous)?, 403);
        fixture.database.execute("DELETE FROM access_keys", [])?;

        status(
            &fixture.exchange(&request("GET", "/photos/object?attributes", "", &headers))?,
            403,
        );

        Ok(())
    })
}

#[test]
fn part_reads_keep_condition_precedence_empty_status_overrides_and_head_errors() -> TestResult {
    with_fixture(|fixture| {
        policy(
            fixture,
            &json!({"Effect":"Allow", "Action":["s3:GetObject", "s3:PutObject", "s3:PutBucketCORS"], "Resource":"*"}),
        )?;

        seed_contents(
            fixture,
            "composite",
            ChecksumAlgorithm::Sha256,
            ChecksumType::Composite,
            b"abc",
        )?;

        let cors = concat!(
            "<CORSConfiguration><CORSRule><AllowedOrigin>https://app.example</AllowedOrigin>",
            "<AllowedMethod>GET</AllowedMethod><AllowedMethod>HEAD</AllowedMethod>",
            "<ExposeHeader>Content-Range</ExposeHeader><ExposeHeader>x-amz-mp-parts-count</ExposeHeader>",
            "<ExposeHeader>x-amz-checksum-sha256</ExposeHeader></CORSRule></CORSConfiguration>"
        );

        status(
            &fixture.exchange(&request("PUT", "/photos?cors", cors, &[]))?,
            200,
        );

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let response = fixture.exchange(&request(
                method,
                "/photos/composite?partNumber=1",
                "",
                &[
                    ("origin", "https://app.example"),
                    ("x-amz-checksum-mode", "ENABLED"),
                ],
            ))?;

            status(&response, 206);
            assert!(response.contains("access-control-allow-origin: https://app.example\r\n"));
            assert!(response.contains("access-control-expose-headers: Content-Range, x-amz-mp-parts-count, x-amz-checksum-sha256\r\n"));
            assert!(response.contains("content-range: bytes 0-2/3\r\n"));
            assert!(response.contains("x-amz-mp-parts-count: 1\r\n"));

            assert!(response.contains(
                "x-amz-checksum-sha256: ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=\r\n"
            ));
        }

        for (key, bytes) in [("ordinary", "abc"), ("empty", "")] {
            report_case(format_args!("key={key:?}"));

            status(
                &fixture.exchange(&request("PUT", &format!("/photos/{key}"), bytes, &[]))?,
                200,
            );
        }

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let empty =
                fixture.exchange(&request(method, "/photos/empty?partNumber=1", "", &[]))?;

            status(&empty, 206);
            assert!(empty.contains("content-length: 0\r\n"));
            assert!(!empty.contains("content-range:"));
            assert_eq!(empty.split_once("\r\n\r\n").unwrap().1, "");

            for (number, expected) in [
                ("0", 400),
                ("10001", 400),
                ("18446744073709551616", 400),
                ("invalid", 400),
                ("2", 416),
            ] {
                report_case(format_args!(
                    "method={method:?}, number={number:?}, expected={expected:?}"
                ));

                let response = fixture.exchange(&request(
                    method,
                    &format!("/photos/ordinary?partNumber={number}"),
                    "",
                    &[],
                ))?;

                status(&response, expected);

                if method == "HEAD" {
                    assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
                }
            }

            for (condition, value, expected) in [
                ("if-none-match", "*", 304),
                ("if-match", "\"different\"", 412),
            ] {
                report_case(format_args!(
                    "method={method:?}, condition={condition:?}, value={value:?}, expected={expected:?}"
                ));

                status(
                    &fixture.exchange(&request(
                        method,
                        "/photos/ordinary?partNumber=2",
                        "",
                        &[(condition, value)],
                    ))?,
                    expected,
                );
            }

            let target = "/photos/ordinary?partNumber=1&response-content-type=application%2Fpdf&response-content-encoding=identity&response-content-disposition=attachment&response-content-language=&response-cache-control=no-store&response-expires=Wed%2C%2021%20Oct%202037%2007%3A28%3A00%20GMT";

            let response =
                fixture.exchange(&request_with_mode(method, target, "", &[], "query"))?;

            status(&response, 206);

            for header in [
                "content-type: application/pdf\r\n",
                "content-encoding: identity\r\n",
                "content-disposition: attachment\r\n",
                "content-language: \r\n",
                "cache-control: no-store\r\n",
                "expires: Wed, 21 Oct 2037 07:28:00 GMT\r\n",
            ] {
                assert!(
                    response.contains(header),
                    "{method}, missing {header:?}: {response}"
                );
            }

            let signed = String::from_utf8(request_with_mode(method, target, "", &[], "query"))?;

            status(
                &fixture.exchange(signed.replace("partNumber=1", "partNumber=2").as_bytes())?,
                403,
            );
        }

        policy(
            fixture,
            &json!({"Effect":"Allow", "Action":"s3:ListBucket", "Resource":"*"}),
        )?;

        for method in ["GET", "HEAD"] {
            report_case(format_args!("method={method:?}"));

            let denied = fixture.exchange(&request(
                method,
                "/photos/ordinary?partNumber=2",
                "",
                &[("if-none-match", "*")],
            ))?;

            status(&denied, 403);
            assert!(!denied.contains("ActualPartCount"));
        }

        Ok(())
    })
}

#[test]
fn all_checksum_modes_project_independent_object_and_part_vectors() -> TestResult {
    with_fixture(|fixture| {
        policy(
            fixture,
            &json!({"Effect":"Allow", "Action":["s3:GetObject", "s3:GetObjectAttributes"], "Resource":"*"}),
        )?;

        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../tests/contract/multipart-completion.json"
        ))?;

        for (index, vector) in vectors["vectors"]
            .as_array()
            .ok_or("missing vectors")?
            .iter()
            .enumerate()
        {
            report_case(format_args!("index={index:?}"));

            let algorithm = vector["algorithm"].as_str().ok_or("missing algorithm")?;
            let mode = vector["mode"].as_str().ok_or("missing mode")?;
            let bytes = vector["body"].as_str().ok_or("missing body")?.as_bytes();
            let checksum = vector["checksum"].as_str().ok_or("missing checksum")?;
            let composite = mode == "COMPOSITE";
            let key = format!("vector-{index}");

            seed_contents(
                fixture,
                &key,
                ChecksumAlgorithm::parse(algorithm)?,
                if composite {
                    ChecksumType::Composite
                } else {
                    ChecksumType::FullObject
                },
                bytes,
            )?;

            let response = fixture.exchange(&request(
                "GET",
                &format!("/photos/{key}?attributes"),
                "",
                &[("x-amz-object-attributes", "Checksum,ObjectParts")],
            ))?;

            status(&response, 200);
            let object_digest = checksum.strip_suffix("-1").unwrap_or(checksum);

            assert!(
                response.contains(&format!(
                    "<Checksum{algorithm}>{object_digest}</Checksum{algorithm}>"
                )),
                "{response}"
            );

            assert!(response.contains(&format!("<ChecksumType>{mode}</ChecksumType>")));
            assert_eq!(response.contains("<Part>"), composite);

            let part_digest = if bytes.is_empty() {
                "AAAAAAAAAAA="
            } else {
                match algorithm {
                    "CRC32" => "NSRBwg==",
                    "CRC32C" => "Nks/tw==",
                    "CRC64NVME" => "BeXKuz/B+us=",
                    "SHA1" => "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=",
                    "SHA256" => "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=",
                    _ => return Err("unknown independent algorithm".into()),
                }
            };

            for method in ["GET", "HEAD"] {
                report_case(format_args!("index={index:?}, method={method:?}"));

                let response = fixture.exchange(&request(
                    method,
                    &format!("/photos/{key}?partNumber=1"),
                    "",
                    &[("x-amz-checksum-mode", "ENABLED")],
                ))?;

                status(&response, 206);

                assert!(
                    response.contains(&format!(
                        "x-amz-checksum-{}: {part_digest}\r\n",
                        algorithm.to_ascii_lowercase()
                    )),
                    "{response}"
                );

                assert!(response.contains(&format!("x-amz-checksum-type: {mode}\r\n")));

                assert_eq!(
                    response.split_once("\r\n\r\n").unwrap().1.as_bytes(),
                    if method == "GET" { bytes } else { b"" }
                );
            }
        }

        Ok(())
    })
}
