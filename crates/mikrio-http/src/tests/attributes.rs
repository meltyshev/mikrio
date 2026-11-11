//! Fixed metadata projections through the actual Attributes route and codecs.

use s3s::dto::{
    Checksum, GetObjectAttributesInput, GetObjectAttributesOutput, GetObjectAttributesParts,
    ObjectPart, Timestamp, TimestampFormat,
};

use super::*;

pub(super) fn response(
    request: S3Request<GetObjectAttributesInput>,
) -> S3Result<S3Response<GetObjectAttributesOutput>> {
    for name in [
        "if-match",
        "if-none-match",
        "if-modified-since",
        "if-unmodified-since",
    ] {
        if request.headers.contains_key(name) {
            return Err(s3s::s3_error!(NotImplemented));
        }
    }

    let input = request.input;

    if input.object_attributes.is_empty() {
        return Err(s3s::s3_error!(InvalidArgument));
    }

    let mut output = GetObjectAttributesOutput {
        last_modified: Some(
            Timestamp::parse(TimestampFormat::DateTime, "2026-10-09T00:00:00Z").unwrap(),
        ),
        ..GetObjectAttributesOutput::default()
    };

    let mut selected = Vec::with_capacity(input.object_attributes.len());

    for attribute in &input.object_attributes {
        report_case(format_args!("attribute={attribute:?}"));

        if selected.contains(&attribute.as_str()) {
            return Err(s3s::s3_error!(InvalidArgument));
        }

        selected.push(attribute.as_str());

        match attribute.as_str() {
            "ETag" => output.e_tag = Some("\"25f9e794323b453885f5181f1b624d0b\"".parse().unwrap()),
            "ObjectSize" => output.object_size = Some(9),
            "StorageClass" => {
                output.storage_class = Some(s3s::dto::StorageClass::from_static("STANDARD"));
            }
            "Checksum" => {
                output.checksum = Some(Checksum {
                    checksum_crc32: Some("y/Q5Jg==".into()),
                    checksum_type: Some(s3s::dto::ChecksumType::from_static("FULL_OBJECT")),
                    ..Checksum::default()
                });
            }
            "ObjectParts" => {
                output.object_parts = Some(if input.key == "count-only" {
                    GetObjectAttributesParts {
                        total_parts_count: Some(1),
                        ..GetObjectAttributesParts::default()
                    }
                } else {
                    assert_eq!(input.max_parts, Some(1));
                    assert_eq!(input.part_number_marker, Some(0));

                    GetObjectAttributesParts {
                        total_parts_count: Some(1),
                        part_number_marker: Some(0),
                        max_parts: Some(1),
                        is_truncated: Some(false),
                        parts: Some(vec![ObjectPart {
                            part_number: Some(1),
                            size: Some(9),
                            checksum_crc32: Some("y/Q5Jg==".into()),
                            ..ObjectPart::default()
                        }]),
                        ..GetObjectAttributesParts::default()
                    }
                });
            }
            _ => return Err(s3s::s3_error!(InvalidArgument)),
        }
    }

    Ok(S3Response::new(output))
}

fn request(mode: &str) -> Vec<u8> {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract/http_sigv4.py");

    let output = crate::tests::python::command()
        .arg(script)
        .args(["--attributes", mode])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "independent Attributes producer failed"
    );

    output.stdout
}

struct Projection(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl s3s::S3 for Projection {
    async fn get_object_attributes(
        &self,
        request: S3Request<GetObjectAttributesInput>,
    ) -> S3Result<S3Response<GetObjectAttributesOutput>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        response(request)
    }
}

#[test]
fn attributes_require_both_current_grants_and_either_explicit_deny_wins() -> TestResult {
    Fixture::run(false, |fixture| {
        let calls = Arc::new(AtomicUsize::new(0));

        fixture.service = Arc::new(Service::with_backend(
            fixture.connection.clone(),
            None,
            Projection(Arc::clone(&calls)),
        )?);

        for (actions, allowed) in [
            (None, false),
            (Some(r#"["s3:GetObject"]"#), false),
            (Some(r#"["s3:GetObjectAttributes"]"#), false),
            (Some(r#"["s3:GetObject","s3:GetObjectAttributes"]"#), true),
        ] {
            report_case(format_args!("actions={actions:?}, allowed={allowed:?}"));

            let policy = actions.map(|actions| {
                format!(
                    r#"{{"Version":"2012-10-17","Statement":{{"Effect":"Allow","Action":{actions},"Resource":"*"}}}}"#
                )
            });

            fixture.database.execute(
                "UPDATE principals SET policy_generation=CASE WHEN ?1 IS NULL THEN NULL ELSE randomblob(16) END, policy_json=?1",
                [policy],
            )?;

            let before = calls.load(Ordering::SeqCst);
            let result = fixture.exchange(&request("etag"))?;
            status(&result, if allowed { 200 } else { 403 });
            assert_eq!(calls.load(Ordering::SeqCst), before + usize::from(allowed));
        }

        for denied_action in ["s3:GetObject", "s3:GetObjectAttributes"] {
            report_case(format_args!("denied_action={denied_action:?}"));

            let policy = format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:GetObject","s3:GetObjectAttributes"],"Resource":"*"}},{{"Effect":"Deny","Action":"{denied_action}","Resource":"*"}}]}}"#
            );

            fixture.database.execute(
                "UPDATE principals SET policy_generation=randomblob(16), policy_json=?1",
                [policy],
            )?;

            status(&fixture.exchange(&request("etag"))?, 403);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }

        Ok(())
    })
}

#[test]
fn attributes_codec_preserves_selection_count_only_and_part_field_names() -> TestResult {
    let fixture = phases::fixture()?;

    for (mode, present, absent) in [
        (
            "etag",
            "<ETag>25f9e794323b453885f5181f1b624d0b</ETag>",
            "<ObjectSize>",
        ),
        ("size", "<ObjectSize>9</ObjectSize>", "<ETag>"),
        ("count-only", "<PartsCount>1</PartsCount>", "<Part>"),
        (
            "part",
            "<PartNumber>1</PartNumber>",
            "<NextPartNumberMarker>",
        ),
    ] {
        report_case(format_args!(
            "mode={mode:?}, present={present:?}, absent={absent:?}"
        ));

        let (response, outcome) = exchange(&fixture.runtime, &fixture.service, &request(mode))?;
        outcome?;
        assert!(response.starts_with("HTTP/1.1 200 "), "{mode}: {response}");
        assert!(response.contains(present), "{mode}: {response}");
        assert!(!response.contains(absent), "{mode}: {response}");
        assert!(response.contains("last-modified: Fri, 09 Oct 2026 00:00:00 GMT"));
    }

    for mode in ["unknown", "missing", "empty", "duplicate", "empty-element"] {
        report_case(format_args!("mode={mode:?}"));

        let (response, outcome) = exchange(&fixture.runtime, &fixture.service, &request(mode))?;
        outcome?;
        status(&response, 400);
    }

    let (response, outcome) = exchange(&fixture.runtime, &fixture.service, &request("condition"))?;
    outcome?;
    status(&response, 501);
    assert!(response.contains("<Code>NotImplemented</Code>"));

    Ok(())
}
