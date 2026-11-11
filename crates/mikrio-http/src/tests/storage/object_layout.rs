//! Real storage projections against independently recorded AWS field expectations.

use mikrio_core::integrity::{ChecksumAlgorithm, ChecksumType, ExpectedDigests};
use mikrio_core::multipart::{
    CompleteRequest, CompletionParts, CreateRequest, MultipartChecksum, PartNumber, PartRequest,
    SelectedPart, UploadRequest,
};
use mikrio_core::object::{Representation, UserMetadata, WriteConditions};

use super::*;

mod matrix;

fn observed(test: &str, response: &str) -> serde_json::Value {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/contract/object-layout.json"
    ))
    .unwrap();

    fixtures["observations"][format!("tests/aws/services/s3/test_s3.py::TestS3::{test}")]["responses"][response].clone()
}

fn seed(fixture: &Fixture<'_>, key: &str, mode: ChecksumType) -> TestResult {
    seed_contents(
        fixture,
        key,
        ChecksumAlgorithm::Crc32c,
        mode,
        b"test content 123",
    )
}

fn seed_contents(
    fixture: &Fixture<'_>,
    key: &str,
    algorithm: ChecksumAlgorithm,
    mode: ChecksumType,
    bytes: &[u8],
) -> TestResult {
    fixture.runtime.block_on(async {
        let object = read(key);

        let upload = fixture
            .connection
            .create_multipart_upload(CreateRequest {
                bucket: object.bucket.clone(),
                key: object.key.clone(),
                expected_owner: None,
                representation: Representation::default(),
                metadata: UserMetadata::default(),
                tags: None,
                checksum: MultipartChecksum::new(algorithm, mode)?,
            })
            .await?;

        let part = fixture
            .connection
            .upload_part(
                PartRequest {
                    bucket: object.bucket.clone(),
                    key: object.key.clone(),
                    upload_id: upload.upload_id.clone(),
                    part_number: PartNumber::new(1)?,
                    expected_owner: None,
                    content_length: None,
                    checksum_algorithm: None,
                    expected: ExpectedDigests::default(),
                },
                std::io::Cursor::new(bytes.to_vec()),
            )
            .await?;

        fixture
            .connection
            .complete_multipart_upload(CompleteRequest {
                upload: UploadRequest {
                    bucket: object.bucket,
                    key: object.key,
                    upload_id: upload.upload_id,
                    expected_owner: None,
                },
                parts: CompletionParts::new([SelectedPart {
                    part_number: PartNumber::new(1)?,
                    etag: part.etag(),
                    checksum: Some(part.checksum()),
                }])?,
                conditions: WriteConditions::default(),
                content_length: None,
                checksum_type: None,
                checksum: None,
            })
            .await?;

        Ok(())
    })
}

#[test]
fn part_get_and_head_match_recorded_status_ranges_checksums_and_errors() -> TestResult {
    with_fixture(|fixture| {
        for (mode, spelling) in [
            (ChecksumType::Composite, "COMPOSITE"),
            (ChecksumType::FullObject, "FULL_OBJECT"),
        ] {
            seed(fixture, spelling, mode)?;

            for (method, snapshot) in [("GET", "get-object-part"), ("HEAD", "head-object-part")] {
                report_case(format_args!(
                    "mode={mode:?}, spelling={spelling:?}, method={method:?}"
                ));

                let expected = observed(
                    &format!("test_get_object_part_checksum[{spelling}]"),
                    snapshot,
                );

                let response = fixture.exchange(&request(
                    method,
                    &format!("/photos/{spelling}?partNumber=1"),
                    "",
                    &[("x-amz-checksum-mode", "ENABLED")],
                ))?;

                status(&response, 206);
                assert_eq!(expected["ResponseMetadata"]["HTTPStatusCode"], 206);

                for (header, field) in [
                    ("content-range", "ContentRange"),
                    ("etag", "ETag"),
                    ("x-amz-checksum-crc32c", "ChecksumCRC32C"),
                    ("x-amz-checksum-type", "ChecksumType"),
                ] {
                    assert!(
                        response.contains(&format!(
                            "{header}: {}\r\n",
                            expected[field].as_str().unwrap()
                        )),
                        "{method}, missing {header:?} ({field}): {response}"
                    );
                }

                assert!(response.contains("x-amz-mp-parts-count: 1\r\n"));
                assert!(response.contains("content-length: 16\r\n"));

                assert_eq!(
                    response.split_once("\r\n\r\n").unwrap().1,
                    if method == "GET" {
                        "test content 123"
                    } else {
                        ""
                    }
                );
            }
        }

        for (path, headers, snapshot) in [
            (
                "/photos/COMPOSITE?partNumber=10",
                vec![],
                "part-doesnt-exist",
            ),
            (
                "/photos/COMPOSITE?partNumber=1",
                vec![("range", "bytes=0-0")],
                "part-with-range",
            ),
        ] {
            report_case(format_args!("path={path:?}, headers={headers:?}"));

            let expected = observed("test_get_object_part", snapshot);
            let response = fixture.exchange(&request("GET", path, "", &headers))?;

            status(
                &response,
                expected["ResponseMetadata"]["HTTPStatusCode"]
                    .as_u64()
                    .unwrap()
                    .try_into()?,
            );

            assert!(response.contains(&format!(
                "<Code>{}</Code>",
                expected["Error"]["Code"].as_str().unwrap()
            )));

            if snapshot == "part-doesnt-exist" {
                assert!(response.contains("<ActualPartCount>1</ActualPartCount>"));
                assert!(response.contains("<PartNumberRequested>10</PartNumberRequested>"));
            }
        }

        Ok(())
    })
}

#[test]
fn persistent_attributes_preserve_selectors_count_only_pages_and_signed_query_fields() -> TestResult
{
    with_fixture(|fixture| {
        fixture.database.execute("UPDATE principals SET policy_generation=randomblob(16), policy_json=?1", [r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":["s3:GetObject","s3:GetObjectAttributes","s3:PutObject"],"Resource":"*"}}"#])?;

        status(
            &fixture.exchange(&request("PUT", "/photos/ordinary", "test-123", &[]))?,
            200,
        );

        for (mode, spelling) in [
            (ChecksumType::Composite, "COMPOSITE"),
            (ChecksumType::FullObject, "FULL_OBJECT"),
        ] {
            report_case(format_args!("mode={mode:?}, spelling={spelling:?}"));

            seed(fixture, spelling, mode)?;

            let response = fixture.exchange(&request(
                "GET",
                &format!("/photos/{spelling}?attributes"),
                "",
                &[
                    (
                        "x-amz-object-attributes",
                        "ETag,Checksum,ObjectParts,ObjectSize,StorageClass",
                    ),
                    ("x-amz-max-parts", "1"),
                ],
            ))?;

            status(&response, 200);
            assert!(response.contains("<ETag>b972025bc34adf8d76d6d51e93c035cc-1</ETag>"));
            assert!(response.contains("<ObjectSize>16</ObjectSize>"));
            assert!(response.contains("<PartsCount>1</PartsCount>"));
            assert!(response.contains("<StorageClass>STANDARD</StorageClass>"));
            assert_eq!(response.contains("<Part>"), mode == ChecksumType::Composite);

            assert_eq!(
                response.contains("<MaxParts>"),
                mode == ChecksumType::Composite
            );

            if mode == ChecksumType::Composite {
                assert!(response.contains("<ChecksumCRC32C>fwuu0Q==</ChecksumCRC32C>"));
                assert!(response.contains("<NextPartNumberMarker>1</NextPartNumberMarker>"));
                assert!(response.contains("<IsTruncated>false</IsTruncated>"));
            }
        }

        let response = fixture.exchange(&request_with_mode("GET", "/photos/ordinary?attributes&x-amz-object-attributes=ETag%2CObjectSize&x-amz-max-parts=1&x-amz-part-number-marker=0", "", &[], "query"))?;
        status(&response, 200);
        assert!(response.contains("<ObjectSize>8</ObjectSize>"));
        assert!(!response.contains("<Checksum>"));
        assert!(!response.contains("<ObjectParts>"));

        for (name, value, code) in [
            ("x-amz-object-attributes", "", 400),
            ("x-amz-object-attributes", "Unknown", 400),
            ("x-amz-object-attributes", "ETag,ETag", 400),
            ("x-amz-max-parts", "0", 400),
            ("x-amz-max-parts", "1001", 400),
            ("x-amz-part-number-marker", "10001", 400),
            ("if-match", "*", 501),
        ] {
            report_case(format_args!(
                "name={name:?}, value={value:?}, code={code:?}"
            ));

            let headers = if name == "x-amz-object-attributes" {
                vec![(name, value)]
            } else {
                vec![("x-amz-object-attributes", "ETag"), (name, value)]
            };

            status(
                &fixture.exchange(&request("GET", "/photos/ordinary?attributes", "", &headers))?,
                code,
            );
        }

        let response = fixture.exchange(&request_with_mode(
            "GET",
            "/photos/ordinary?attributes&x-amz-object-attributes=ObjectSize",
            "",
            &[("x-amz-object-attributes", "ETag")],
            "query",
        ))?;

        // The producer signs only host in query mode; the extra x-amz header
        // must fail authentication before alias resolution.
        status(&response, 403);

        let response = fixture.exchange(&request(
            "GET",
            "/photos/ordinary?attributes&x-amz-object-attributes=ObjectSize",
            "",
            &[("x-amz-object-attributes", "ETag")],
        ))?;

        status(&response, 400);
        Ok(())
    })
}
