//! Test-only observation sink conversion. It owns no payload file or publication.

use mikrio_core::integrity::{
    Checksum, ChecksumAlgorithm, ExpectedDigests, IntegrityError, PayloadFacts, PayloadVerifier,
};

use super::*;

fn error(error: IntegrityError) -> s3s::S3Error {
    match error {
        IntegrityError::UnsupportedAlgorithm => s3s::s3_error!(NotImplemented),
        IntegrityError::AlgorithmMismatch | IntegrityError::DigestMismatch => {
            s3s::s3_error!(BadDigest)
        }
        IntegrityError::InvalidChecksumType | IntegrityError::InvalidPartCount => {
            s3s::s3_error!(InvalidRequest)
        }
        IntegrityError::InvalidDigest => s3s::s3_error!(InvalidDigest),
        IntegrityError::LengthMismatch => s3s::s3_error!(IncompleteBody),
        IntegrityError::TooLarge => s3s::s3_error!(EntityTooLarge),
    }
}

pub(super) async fn consume(mut request: S3Request<PutObjectInput>) -> S3Result<PayloadFacts> {
    let input = &request.input;

    if input.checksum_md5.is_some()
        || input.checksum_sha512.is_some()
        || input.checksum_xxhash128.is_some()
        || input.checksum_xxhash3.is_some()
        || input.checksum_xxhash64.is_some()
    {
        return Err(s3s::s3_error!(NotImplemented));
    }

    let mut selected = None;

    for (algorithm, value) in [
        (ChecksumAlgorithm::Crc32, input.checksum_crc32.as_deref()),
        (ChecksumAlgorithm::Crc32c, input.checksum_crc32c.as_deref()),
        (
            ChecksumAlgorithm::Crc64Nvme,
            input.checksum_crc64nvme.as_deref(),
        ),
        (ChecksumAlgorithm::Sha1, input.checksum_sha1.as_deref()),
        (ChecksumAlgorithm::Sha256, input.checksum_sha256.as_deref()),
    ] {
        let Some(value) = value else { continue };

        if selected
            .replace(Checksum::parse(algorithm, value).map_err(error)?)
            .is_some()
        {
            return Err(s3s::s3_error!(InvalidRequest));
        }
    }

    let trailer = request
        .headers
        .get("x-amz-trailer")
        .map(|value| {
            let name = value.to_str().map_err(|_| s3s::s3_error!(InvalidRequest))?;

            let algorithm = match name {
                "x-amz-checksum-crc32" => ChecksumAlgorithm::Crc32,
                "x-amz-checksum-crc32c" => ChecksumAlgorithm::Crc32c,
                "x-amz-checksum-crc64nvme" => ChecksumAlgorithm::Crc64Nvme,
                "x-amz-checksum-sha1" => ChecksumAlgorithm::Sha1,
                "x-amz-checksum-sha256" => ChecksumAlgorithm::Sha256,
                _ => return Err(s3s::s3_error!(NotImplemented)),
            };

            Ok((name, algorithm))
        })
        .transpose()?;

    let declared_algorithm = input
        .checksum_algorithm
        .as_ref()
        .map(|value| ChecksumAlgorithm::parse(value.as_str()))
        .transpose()
        .map_err(error)?;

    if declared_algorithm.is_some() && selected.is_none() && trailer.is_none() {
        return Err(s3s::s3_error!(InvalidRequest));
    }

    let algorithm = selected
        .map(Checksum::algorithm)
        .or(trailer.map(|(_, algorithm)| algorithm))
        .or(declared_algorithm)
        .unwrap_or(ChecksumAlgorithm::Crc64Nvme);

    if declared_algorithm.is_some_and(|declared| declared != algorithm)
        || trailer.is_some_and(|(_, trailing)| trailing != algorithm)
    {
        return Err(s3s::s3_error!(BadDigest));
    }

    let expected = ExpectedDigests::parse(input.content_md5.as_deref(), selected).map_err(error)?;

    // aws-chunked Content-Length is the encoded envelope; only this explicit
    // decoded length describes payload. Ordinary HTTP retains its own length.
    let declared = if let Some(length) = request.headers.get("x-amz-decoded-content-length") {
        Some(
            length
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| s3s::s3_error!(InvalidArgument))?,
        )
    } else {
        input
            .content_length
            .map(u64::try_from)
            .transpose()
            .map_err(|_| s3s::s3_error!(InvalidArgument))?
    };

    let mut verifier = PayloadVerifier::new(declared, 5 * 1024 * 1024 * 1024, algorithm, expected)
        .map_err(error)?;

    let mut body = request
        .input
        .body
        .take()
        .ok_or_else(|| s3s::s3_error!(InvalidRequest))?;

    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|_| s3s::s3_error!(BadDigest))?;
        verifier.update(&chunk).map_err(error)?;
    }

    let terminal = if let Some((name, algorithm)) = trailer {
        let headers = request
            .trailing_headers
            .as_ref()
            .and_then(s3s::TrailingHeaders::take)
            .ok_or_else(|| s3s::s3_error!(InvalidRequest))?;

        if headers.get_all(name).iter().count() != 1 {
            return Err(s3s::s3_error!(InvalidRequest));
        }

        let value = headers
            .get(name)
            .ok_or_else(|| s3s::s3_error!(InvalidRequest))?
            .to_str()
            .map_err(|_| s3s::s3_error!(InvalidDigest))?;

        ExpectedDigests::parse(
            None,
            Some(Checksum::parse(algorithm, value).map_err(error)?),
        )
        .map_err(error)?
    } else {
        ExpectedDigests::default()
    };

    verifier.finish(terminal).map_err(error)
}

pub(super) async fn consume_post(request: S3Request<PostObjectInput>) -> S3Result<PayloadFacts> {
    consume(request.map_input(|input| PutObjectInput {
        bucket: input.bucket,
        key: input.key,
        body: input.body,
        content_length: input.content_length,
        content_md5: input.content_md5,
        checksum_algorithm: input.checksum_algorithm,
        checksum_crc32: input.checksum_crc32,
        checksum_crc32c: input.checksum_crc32c,
        checksum_crc64nvme: input.checksum_crc64nvme,
        checksum_md5: input.checksum_md5,
        checksum_sha1: input.checksum_sha1,
        checksum_sha256: input.checksum_sha256,
        checksum_sha512: input.checksum_sha512,
        checksum_xxhash128: input.checksum_xxhash128,
        checksum_xxhash3: input.checksum_xxhash3,
        checksum_xxhash64: input.checksum_xxhash64,
        ..PutObjectInput::default()
    }))
    .await
}

pub(super) fn raw(mode: &str) -> Vec<u8> {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract/http_sigv4.py");

    let output = crate::tests::python::command()
        .arg(script)
        .args(["--integrity", mode])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "independent integrity producer failed"
    );

    output.stdout
}

#[test]
fn crc32_headers_and_trailers_finish_only_after_verified_terminal_bytes() -> TestResult {
    Fixture::run(false, |fixture| {
        for mode in ["header", "trailer", "trailer-lf", "empty"] {
            report_case(format_args!("mode={mode:?}"));

            status(&fixture.exchange(&raw(mode))?, 200);
        }

        for mode in [
            "bad-header",
            "bad-md5",
            "bad-trailer",
            "missing-trailer",
            "extra-decoded",
            "short-decoded",
            "duplicate-trailer",
        ] {
            let response = fixture.exchange(&raw(mode))?;

            assert!(
                response.starts_with("HTTP/1.1 4"),
                "mode {mode}: {}",
                response.lines().next().unwrap_or("empty")
            );

            assert_eq!(fixture.observed.lock().unwrap().finishes, 4, "mode {mode}");
        }

        assert_eq!(fixture.observed.lock().unwrap().bytes, 27);
        Ok(())
    })
}

#[test]
fn each_flexible_checksum_header_and_trailer_requires_valid_bytes_and_termination() -> TestResult {
    Fixture::run(false, |fixture| {
        let mut finishes = 0;

        for algorithm in ["CRC32", "CRC32C", "CRC64NVME", "SHA1", "SHA256"] {
            for transport in ["header", "trailer"] {
                report_case(format_args!(
                    "algorithm={algorithm:?}, transport={transport:?}"
                ));

                let mode = format!("flex:{algorithm}:{transport}");
                status(&fixture.exchange(&raw(&mode))?, 200);
                finishes += 1;
            }

            for transport in ["header-bad", "trailer-bad", "trailer-truncated"] {
                report_case(format_args!(
                    "algorithm={algorithm:?}, transport={transport:?}"
                ));

                let mode = format!("flex:{algorithm}:{transport}");
                let response = fixture.exchange(&raw(&mode))?;
                status(&response, 400);

                assert_eq!(
                    fixture.observed.lock().unwrap().finishes,
                    finishes,
                    "{mode}"
                );
            }
        }

        assert_eq!(fixture.observed.lock().unwrap().bytes, 90);
        Ok(())
    })
}

#[test]
fn rejected_singletons_close_before_a_pipelined_authenticated_request() -> TestResult {
    Fixture::run(false, |fixture| {
        let valid = get("/photos/next", "header");

        for duplicate in [
            "host: duplicate",
            "authorization: invalid",
            "x-amz-decoded-content-length: 9\r\nx-amz-decoded-content-length: 10",
            "x-amz-checksum-crc32: AAAAAA==\r\nx-amz-checksum-crc32: y/Q5Jg==",
        ] {
            report_case(format_args!("duplicate={duplicate:?}"));

            let request = String::from_utf8(get("/photos/first", "header"))?.replace(
                "connection: close",
                &format!("{duplicate}\r\nconnection: keep-alive"),
            );

            let mut pipeline = request.into_bytes();
            pipeline.extend_from_slice(&valid);
            let response = fixture.exchange(&pipeline)?;
            status(&response, 400);
            assert_eq!(response.matches("HTTP/1.1 ").count(), 1);
            assert!(response.to_ascii_lowercase().contains("connection: close"));

            assert_eq!(
                fixture.observed.lock().unwrap().admitted,
                Vec::<(String, String)>::new()
            );
        }

        Ok(())
    })
}

#[test]
fn truncated_checksum_trailer_cannot_finish_the_payload() -> TestResult {
    Fixture::run(false, |fixture| {
        for mode in [
            "truncated-trailer",
            "missing-final-crlf",
            "missing-completion",
        ] {
            report_case(format_args!("mode={mode:?}"));

            status(&fixture.exchange(&raw(mode))?, 400);
            assert_eq!(fixture.observed.lock().unwrap().finishes, 0, "mode {mode}");
        }

        Ok(())
    })
}

#[test]
fn signed_chunks_require_the_complete_ordered_signature_chain() -> TestResult {
    Fixture::run(false, |fixture| {
        for mode in ["signed-valid", "signed-empty"] {
            let response = fixture.exchange(&raw(mode))?;
            assert!(response.starts_with("HTTP/1.1 200 "), "{mode}: {response}");
        }

        for mode in [
            "signed-bad-data",
            "signed-reordered",
            "signed-bad-completion",
            "signed-missing-completion",
            "signed-truncated",
            "signed-extra-bytes",
        ] {
            report_case(format_args!("mode={mode:?}"));

            let response = fixture.exchange(&raw(mode))?;
            assert!(response.starts_with("HTTP/1.1 4"), "accepted {mode}");
            assert_eq!(fixture.observed.lock().unwrap().finishes, 2);
        }

        assert_eq!(fixture.observed.lock().unwrap().bytes, 9);
        Ok(())
    })
}

#[test]
fn conflicting_lengths_close_before_dispatch_in_either_header_order() -> TestResult {
    Fixture::run(false, |fixture| {
        let valid = get("/photos/next", "header");

        for framing in [
            "content-length: 0\r\ntransfer-encoding: chunked",
            "transfer-encoding: chunked\r\ncontent-length: 0",
        ] {
            report_case(format_args!("framing={framing:?}"));

            let request = String::from_utf8(get("/photos/first", "header"))?
                .replace("content-length: 0", framing)
                .replace("connection: close", "connection: keep-alive");

            let mut pipeline = request.into_bytes();
            pipeline.extend_from_slice(b"0\r\n\r\n");
            pipeline.extend_from_slice(&valid);
            let (response, outcome) = fixture.exchange_outcome(&pipeline)?;
            assert!(outcome.is_err(), "parser rejection closes the connection");
            status(&response, 400);
            assert_eq!(response.matches("HTTP/1.1 ").count(), 1);

            assert_eq!(
                fixture.observed.lock().unwrap().admitted,
                Vec::<(String, String)>::new()
            );
        }

        Ok(())
    })
}
