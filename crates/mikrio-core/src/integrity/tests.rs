use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn put_size_bound_rejects_declared_and_unknown_length_overflow() -> TestResult {
    let integrity = PutIntegrity::new(PutIntegrityInput::default())?;

    for length in [0, 5_368_709_119, 5_368_709_120] {
        assert_eq!(crate::object::ByteLength::for_put(length)?.get(), length);
        assert!(integrity.verifier(Some(length)).is_ok(), "length={length}");
    }

    for length in [5_368_709_121, i64::MAX.unsigned_abs(), u64::MAX] {
        assert_eq!(
            crate::object::ByteLength::for_put(length),
            Err(crate::object::ObjectValueError::PutObjectSize),
            "length={length}"
        );

        assert!(
            matches!(
                integrity.verifier(Some(length)),
                Err(IntegrityError::TooLarge)
            ),
            "length={length}"
        );
    }

    let mut verifier = integrity.verifier(None)?;
    // Set only the byte counter to exercise the exact boundary without a 5 GiB allocation.
    // This does not establish payload hashing or storage capacity at that scale.
    verifier.length = 5_368_709_119;
    verifier.update(b"x")?;
    assert_eq!(verifier.length, 5_368_709_120);
    assert_eq!(verifier.update(b"x"), Err(IntegrityError::TooLarge));
    assert_eq!(verifier.update(b""), Err(IntegrityError::TooLarge));

    assert_eq!(
        verifier.finish(ExpectedDigests::default()),
        Err(IntegrityError::TooLarge)
    );

    Ok(())
}

#[test]
fn multipart_transport_headers_and_trailers_are_verified_without_changing_stored_digest()
-> TestResult {
    let expected = ExpectedDigests::parse(
        Some("kAFQmDzST7DWlj99KOF/cg=="),
        Some(Checksum::parse(ChecksumAlgorithm::Crc32, "NSRBwg==")?),
    )?;

    for terminal in [
        expected,
        ExpectedDigests::parse(None, Some(Checksum::Crc32([0; 4])))?,
        ExpectedDigests::parse(Some("AAAAAAAAAAAAAAAAAAAAAA=="), None)?,
    ] {
        let mut verifier = PayloadVerifier::multipart(
            Some(3),
            3,
            ChecksumAlgorithm::Crc64Nvme,
            expected,
            Some(ChecksumAlgorithm::Crc32),
        )?;

        verifier.update(b"a")?;
        verifier.update(b"bc")?;
        let result = verifier.finish(terminal);

        if terminal.md5 == expected.md5 && terminal.checksum == expected.checksum {
            assert_eq!(result?.checksum().encode(), "BeXKuz/B+us=");
        } else {
            assert_eq!(
                result,
                Err(IntegrityError::DigestMismatch),
                "terminal={terminal:?}"
            );
        }
    }

    Ok(())
}

#[test]
fn each_put_digest_field_selects_and_verifies_its_own_algorithm() -> TestResult {
    for (input, algorithm, encoded) in [
        (
            PutIntegrityInput {
                checksum_crc32: Some("y/Q5Jg==".into()),
                ..PutIntegrityInput::default()
            },
            ChecksumAlgorithm::Crc32,
            "y/Q5Jg==",
        ),
        (
            PutIntegrityInput {
                checksum_crc32c: Some("4waSgw==".into()),
                ..PutIntegrityInput::default()
            },
            ChecksumAlgorithm::Crc32c,
            "4waSgw==",
        ),
        (
            PutIntegrityInput {
                checksum_crc64_nvme: Some("rosUhgp5mIg=".into()),
                ..PutIntegrityInput::default()
            },
            ChecksumAlgorithm::Crc64Nvme,
            "rosUhgp5mIg=",
        ),
        (
            PutIntegrityInput {
                checksum_sha1: Some("98O8HYCOBHMq32eZZczDTKeuNEE=".into()),
                ..PutIntegrityInput::default()
            },
            ChecksumAlgorithm::Sha1,
            "98O8HYCOBHMq32eZZczDTKeuNEE=",
        ),
        (
            PutIntegrityInput {
                checksum_sha256: Some("FeKw08M4keuw8e9gnsQZQgwg4yDOlMZfvIwzEkSOsiU=".into()),
                ..PutIntegrityInput::default()
            },
            ChecksumAlgorithm::Sha256,
            "FeKw08M4keuw8e9gnsQZQgwg4yDOlMZfvIwzEkSOsiU=",
        ),
    ] {
        let integrity = PutIntegrity::new(input)?;

        let mut verifier =
            PayloadVerifier::new(Some(9), 9, integrity.algorithm(), integrity.expected())?;

        verifier.update(b"123456789")?;
        let checksum = verifier.finish(ExpectedDigests::default())?.checksum();
        assert_eq!(checksum.algorithm(), algorithm, "algorithm={algorithm:?}");
        assert_eq!(checksum.encode(), encoded);

        let mut corrupt =
            PayloadVerifier::new(Some(9), 9, integrity.algorithm(), integrity.expected())?;

        corrupt.update(b"123456788")?;

        assert_eq!(
            corrupt.finish(ExpectedDigests::default()),
            Err(IntegrityError::DigestMismatch),
            "{algorithm:?}"
        );
    }

    Ok(())
}

#[test]
fn put_selection_uses_one_algorithm_and_keeps_md5_as_an_independent_expectation() -> TestResult {
    let default = PutIntegrity::new(PutIntegrityInput::default())?;
    let verifier = PayloadVerifier::new(Some(0), 0, default.algorithm(), default.expected())?;
    let facts = verifier.finish(ExpectedDigests::default())?;
    assert_eq!(facts.checksum().algorithm(), ChecksumAlgorithm::Crc64Nvme);
    assert_eq!(facts.checksum().encode(), "AAAAAAAAAAA=");

    for selected in [None, Some(ChecksumAlgorithm::Crc32)] {
        let integrity = PutIntegrity::new(PutIntegrityInput {
            content_md5: Some("JfnnlDI7RTiF9RgfG2JNCw==".into()),
            checksum_algorithm: selected,
            checksum_crc32: Some("y/Q5Jg==".into()),
            ..PutIntegrityInput::default()
        })?;

        let mut verifier =
            PayloadVerifier::new(Some(9), 9, integrity.algorithm(), integrity.expected())?;

        verifier.update(b"123456789")?;

        assert_eq!(
            verifier
                .finish(ExpectedDigests::default())?
                .checksum()
                .encode(),
            "y/Q5Jg=="
        );
    }

    let integrity = PutIntegrity::new(PutIntegrityInput {
        content_md5: Some("1B2M2Y8AsgTpgAmY7PhCfg==".into()),
        checksum_crc32: Some("y/Q5Jg==".into()),
        ..PutIntegrityInput::default()
    })?;

    let mut verifier =
        PayloadVerifier::new(Some(9), 9, integrity.algorithm(), integrity.expected())?;

    verifier.update(b"123456789")?;

    assert_eq!(
        verifier.finish(ExpectedDigests::default()),
        Err(IntegrityError::DigestMismatch)
    );

    Ok(())
}

#[test]
fn put_selection_rejects_conflicting_algorithms_and_malformed_independent_md5() {
    for (case_index, (input, expected_error)) in [
        (
            PutIntegrityInput {
                checksum_algorithm: Some(ChecksumAlgorithm::Sha256),
                checksum_crc32: Some("AAAAAA==".into()),
                ..PutIntegrityInput::default()
            },
            IntegrityError::AlgorithmMismatch,
        ),
        (
            PutIntegrityInput {
                checksum_crc32: Some("AAAAAA==".into()),
                checksum_crc32c: Some("AAAAAA==".into()),
                ..PutIntegrityInput::default()
            },
            IntegrityError::AlgorithmMismatch,
        ),
        (
            PutIntegrityInput {
                content_md5: Some("AAAAAA==".into()),
                checksum_crc32: Some("AAAAAA==".into()),
                ..PutIntegrityInput::default()
            },
            IntegrityError::InvalidDigest,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            PutIntegrity::new(input).unwrap_err(),
            expected_error,
            "case {case_index}"
        );
    }
}

#[test]
fn independent_vectors_agree_for_empty_and_every_chunk_split() -> TestResult {
    // CRC catalog check string, RFC 1321 empty MD5, and independently computed
    // Python hashlib/zlib values; no expected digest comes from this module.
    for (body, md5, crc32, etag) in [
        (
            &b""[..],
            "1B2M2Y8AsgTpgAmY7PhCfg==",
            "AAAAAA==",
            "\"d41d8cd98f00b204e9800998ecf8427e\"",
        ),
        (
            &b"123456789"[..],
            "JfnnlDI7RTiF9RgfG2JNCw==",
            "y/Q5Jg==",
            "\"25f9e794323b453885f5181f1b624d0b\"",
        ),
    ] {
        for split in 0..=body.len() {
            let expected = ExpectedDigests::parse(
                Some(md5),
                Some(Checksum::parse(ChecksumAlgorithm::Crc32, crc32)?),
            )?;

            let mut verifier = PayloadVerifier::new(
                Some(u64::try_from(body.len())?),
                9,
                ChecksumAlgorithm::Crc32,
                expected,
            )?;

            verifier.update(&body[..split])?;
            verifier.update(&[])?;
            verifier.update(&body[split..])?;
            let facts = verifier.finish(expected)?;
            assert_eq!(facts.length(), u64::try_from(body.len())?);

            assert_eq!(
                facts.checksum().encode(),
                crc32,
                "body={body:?}, split={split}"
            );

            assert_eq!(facts.etag(), etag, "body={body:?}, split={split}");
        }
    }

    Ok(())
}

#[test]
fn malformed_checksums_and_noncanonical_padding_are_rejected() {
    for digest in [
        "",
        "AAAAAA",
        "AAAAAA===",
        "AAAAAB==",
        "AAAAAA==\n",
        "----AA==",
        "AAAAAA==-2",
        "éAAAAAA",
    ] {
        assert!(
            matches!(
                Checksum::parse(ChecksumAlgorithm::Crc32, digest),
                Err(IntegrityError::InvalidDigest)
            ),
            "digest={digest:?}"
        );
    }

    assert!(matches!(
        ExpectedDigests::parse(Some("AAAAAA=="), None),
        Err(IntegrityError::InvalidDigest)
    ));
}

#[test]
fn ignored_update_failure_and_short_eof_cannot_produce_facts() -> TestResult {
    let mut verifier = PayloadVerifier::new(
        Some(1),
        2,
        ChecksumAlgorithm::Crc32,
        ExpectedDigests::default(),
    )?;

    assert_eq!(verifier.update(b"ab"), Err(IntegrityError::LengthMismatch));
    assert_eq!(verifier.update(b"a"), Err(IntegrityError::LengthMismatch));

    assert_eq!(
        verifier.finish(ExpectedDigests::default()),
        Err(IntegrityError::LengthMismatch)
    );

    let verifier = PayloadVerifier::new(
        Some(1),
        2,
        ChecksumAlgorithm::Crc32,
        ExpectedDigests::default(),
    )?;

    assert_eq!(
        verifier.finish(ExpectedDigests::default()),
        Err(IntegrityError::LengthMismatch)
    );

    let mut verifier = PayloadVerifier::new(
        None,
        1,
        ChecksumAlgorithm::Crc32,
        ExpectedDigests::default(),
    )?;

    assert_eq!(verifier.update(b"ab"), Err(IntegrityError::TooLarge));

    assert_eq!(
        verifier.finish(ExpectedDigests::default()),
        Err(IntegrityError::TooLarge)
    );

    assert!(matches!(
        PayloadVerifier::new(
            Some(u64::MAX),
            1,
            ChecksumAlgorithm::Crc32,
            ExpectedDigests::default()
        ),
        Err(IntegrityError::TooLarge)
    ));

    Ok(())
}

#[test]
fn header_and_trailer_must_both_match_actual_bytes() -> TestResult {
    let actual = ExpectedDigests::parse(
        None,
        Some(Checksum::parse(ChecksumAlgorithm::Crc32, "y/Q5Jg==")?),
    )?;

    let wrong = ExpectedDigests::parse(
        None,
        Some(Checksum::parse(ChecksumAlgorithm::Crc32, "AAAAAA==")?),
    )?;

    for (header, trailer) in [(wrong, actual), (actual, wrong)] {
        let mut verifier = PayloadVerifier::new(None, 9, ChecksumAlgorithm::Crc32, header)?;
        verifier.update(b"123456789")?;

        assert_eq!(
            verifier.finish(trailer),
            Err(IntegrityError::DigestMismatch),
            "header={header:?}, trailer={trailer:?}"
        );
    }

    let mut verifier = PayloadVerifier::new(Some(9), 9, ChecksumAlgorithm::Crc32, actual)?;
    verifier.update(b"123456780")?;

    assert_eq!(
        verifier.finish(ExpectedDigests::default()),
        Err(IntegrityError::DigestMismatch)
    );

    Ok(())
}

#[test]
fn repeated_borrowed_buffer_does_not_become_retained_payload() -> TestResult {
    let mut verifier = PayloadVerifier::new(
        None,
        16 * 1024 * 1024,
        ChecksumAlgorithm::Crc32,
        ExpectedDigests::default(),
    )?;

    let mut buffer = vec![0; 64 * 1024];

    for value in 0..=255 {
        buffer.fill(value);
        verifier.update(&buffer)?;
    }

    buffer.fill(0);
    let facts = verifier.finish(ExpectedDigests::default())?;
    assert_eq!(facts.length(), 16 * 1024 * 1024);
    // Independent hashlib/zlib loop retained in the integrity evidence.
    assert_eq!(facts.etag(), "\"f9dd36d65912f889837c27b4973061f9\"");
    Ok(())
}

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../tests/contract/checksum-vectors.json"
    ))
    .unwrap()
}

fn body_bytes(encoded: &str) -> Vec<u8> {
    let (pairs, remainder) = encoded.as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty(), "incomplete fixture byte");

    pairs
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn every_algorithm_matches_independent_vectors_at_every_chunk_boundary() -> TestResult {
    for vector in fixture()["vectors"].as_array().unwrap() {
        let body = body_bytes(vector["body_hex"].as_str().unwrap());

        for (name, encoded) in vector["checksums"].as_object().unwrap() {
            let algorithm = ChecksumAlgorithm::parse(name)?;
            let expected = Checksum::parse(algorithm, encoded.as_str().unwrap())?;

            for split in 0..=body.len() {
                let expectations = ExpectedDigests::parse(None, Some(expected))?;

                let mut verifier = PayloadVerifier::new(
                    Some(u64::try_from(body.len())?),
                    256,
                    algorithm,
                    expectations,
                )?;

                verifier.update(&body[..split])?;
                verifier.update(&[])?;
                verifier.update(&body[split..])?;
                let facts = verifier.finish(expectations)?;

                assert_eq!(
                    facts.checksum(),
                    expected,
                    "{name}, body={body:?}, split={split}"
                );

                assert_eq!(facts.checksum().encode(), encoded.as_str().unwrap());
            }
        }
    }

    Ok(())
}

#[test]
fn every_algorithm_rejects_corruption_and_conflicting_terminal_expectations() -> TestResult {
    let fixtures = fixture();
    let empty = fixtures["vectors"][0]["checksums"].as_object().unwrap();

    for (name, encoded) in fixtures["vectors"][1]["checksums"].as_object().unwrap() {
        let algorithm = ChecksumAlgorithm::parse(name)?;
        let expected = Checksum::parse(algorithm, encoded.as_str().unwrap())?;
        let wrong = Checksum::parse(algorithm, empty[name].as_str().unwrap())?;

        for (header, trailer, body) in [
            (expected, wrong, &b"123456789"[..]),
            (wrong, expected, &b"123456789"[..]),
            (expected, expected, &b"123456780"[..]),
        ] {
            let mut verifier = PayloadVerifier::new(
                Some(9),
                9,
                algorithm,
                ExpectedDigests::parse(None, Some(header))?,
            )?;

            verifier.update(body)?;

            assert_eq!(
                verifier.finish(ExpectedDigests::parse(None, Some(trailer))?),
                Err(IntegrityError::DigestMismatch),
                "{name}, header={header:?}, trailer={trailer:?}, body={body:?}"
            );
        }
    }

    Ok(())
}

#[test]
fn algorithm_mismatches_never_fall_back_to_another_digest() -> TestResult {
    let crc32 = Checksum::Crc32([0; 4]);
    let expected = ExpectedDigests::parse(None, Some(crc32))?;

    assert!(matches!(
        PayloadVerifier::new(None, 9, ChecksumAlgorithm::Sha256, expected),
        Err(IntegrityError::AlgorithmMismatch)
    ));

    let verifier = PayloadVerifier::new(
        None,
        9,
        ChecksumAlgorithm::Sha256,
        ExpectedDigests::default(),
    )?;

    assert_eq!(
        verifier.finish(expected),
        Err(IntegrityError::AlgorithmMismatch)
    );

    for name in ["crc32", "MD5", "SHA512", "XXHASH64", ""] {
        assert_eq!(
            ChecksumAlgorithm::parse(name),
            Err(IntegrityError::UnsupportedAlgorithm),
            "algorithm={name:?}"
        );
    }

    Ok(())
}

#[test]
fn composite_values_hash_ordered_binary_part_digests_and_roundtrip_the_count() -> TestResult {
    let fixtures = fixture();

    for (name, encoded) in fixtures["composite"].as_object().unwrap() {
        let algorithm = ChecksumAlgorithm::parse(name)?;

        let parts = [3, 4].map(|index| {
            Checksum::parse(
                algorithm,
                fixtures["vectors"][index]["checksums"][name]
                    .as_str()
                    .unwrap(),
            )
            .unwrap()
        });

        let checksum = ObjectChecksum::composite(algorithm, parts)?;
        assert_eq!(checksum.encode(), encoded.as_str().unwrap(), "{name}");
        assert_eq!(checksum.part_count(), Some(2));
        assert_eq!(checksum.checksum_type(), ChecksumType::Composite);

        assert_eq!(
            ObjectChecksum::parse(algorithm, ChecksumType::Composite, &checksum.encode())?,
            checksum
        );

        assert_ne!(
            ObjectChecksum::composite(algorithm, parts.into_iter().rev())?,
            checksum
        );

        assert!(
            ObjectChecksum::parse(algorithm, ChecksumType::FullObject, &checksum.encode()).is_err()
        );
    }

    Ok(())
}

#[test]
fn multipart_modes_and_count_encodings_are_checked_before_use() -> TestResult {
    use ChecksumAlgorithm::{Crc32, Crc32c, Crc64Nvme, Sha1, Sha256};

    for (algorithm, full, composite) in [
        (Crc32, true, true),
        (Crc32c, true, true),
        (Crc64Nvme, true, false),
        (Sha1, false, true),
        (Sha256, false, true),
    ] {
        assert_eq!(
            algorithm.supports_multipart(ChecksumType::FullObject),
            full,
            "{algorithm:?}"
        );

        assert_eq!(
            algorithm.supports_multipart(ChecksumType::Composite),
            composite,
            "{algorithm:?}"
        );
    }

    for value in [
        "AAAAAA==",
        "AAAAAA==-0",
        "AAAAAA==-01",
        "AAAAAA==--1",
        "AAAAAA==-+1",
        "AAAAAA==-10001",
        "AAAAAA==-1-2",
        "AAAAAA==-1 ",
    ] {
        assert!(
            ObjectChecksum::parse(Crc32, ChecksumType::Composite, value).is_err(),
            "{value}"
        );
    }

    let part = Checksum::Crc32([0; 4]);

    assert_eq!(
        ObjectChecksum::composite(Crc64Nvme, [Checksum::Crc64Nvme([0; 8])]),
        Err(IntegrityError::InvalidChecksumType)
    );

    assert_eq!(
        ObjectChecksum::composite(Crc32, []),
        Err(IntegrityError::InvalidPartCount)
    );

    assert_eq!(
        ObjectChecksum::composite(Crc32, [part, Checksum::Crc32c([0; 4])]),
        Err(IntegrityError::AlgorithmMismatch)
    );

    assert_eq!(
        ObjectChecksum::composite(Crc32, std::iter::repeat_n(part, 10_001)),
        Err(IntegrityError::InvalidPartCount)
    );

    let maximum = ObjectChecksum::composite(Crc32, std::iter::repeat_n(part, 10_000))?;

    assert_eq!(
        ObjectChecksum::parse(Crc32, ChecksumType::Composite, &maximum.encode())?,
        maximum
    );

    Ok(())
}
