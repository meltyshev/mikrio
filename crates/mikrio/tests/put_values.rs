use std::error::Error as _;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use mikrio::{BodySource, ChecksumAlgorithm, ErrorCode, PutObjectInput, Tag, Timestamp};
use tokio::io::{AsyncRead, AsyncReadExt as _, ReadBuf};

struct ObservedSource {
    poll_count: Arc<AtomicUsize>,
    drop_count: Arc<AtomicUsize>,
}

impl AsyncRead for ObservedSource {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.poll_count.fetch_add(1, Ordering::Relaxed);
        Poll::Ready(Err(io::Error::from_raw_os_error(12345)))
    }
}

impl Drop for ObservedSource {
    fn drop(&mut self) {
        self.drop_count.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn put_tags_preserve_omission_empty_values_and_sorted_case_sensitive_keys() {
    let omitted = PutObjectInput::builder("photos", "key", Vec::new())
        .build()
        .unwrap();

    let empty = PutObjectInput::builder("photos", "key", Vec::new())
        .tags(Vec::new())
        .build()
        .unwrap();

    assert_eq!(omitted.tags(), None);
    assert_eq!(empty.tags(), Some([].as_slice()));

    let input = PutObjectInput::builder("photos", "key", Vec::new())
        .tags(vec![
            Tag {
                key: "name".into(),
                value: "日本語".into(),
            },
            Tag {
                key: "Name".into(),
                value: String::new(),
            },
        ])
        .build()
        .unwrap();

    assert_eq!(
        input.tags(),
        Some(
            [
                Tag {
                    key: "Name".into(),
                    value: String::new()
                },
                Tag {
                    key: "name".into(),
                    value: "日本語".into()
                },
            ]
            .as_slice()
        )
    );
}

#[test]
fn invalid_put_tags_release_the_source_without_polling_or_losing_the_cause() {
    for (case, tags) in [
        vec![
            Tag {
                key: "name".into(),
                value: "first".into(),
            },
            Tag {
                key: "name".into(),
                value: "second".into(),
            },
        ],
        vec![Tag {
            key: "𐐀".repeat(65),
            value: String::new(),
        }],
        vec![Tag {
            key: "name".into(),
            value: "\n".into(),
        }],
    ]
    .into_iter()
    .enumerate()
    {
        let poll_count = Arc::new(AtomicUsize::new(0));
        let drop_count = Arc::new(AtomicUsize::new(0));

        let source = BodySource::from_reader(ObservedSource {
            poll_count: Arc::clone(&poll_count),
            drop_count: Arc::clone(&drop_count),
        });

        let error = PutObjectInput::builder("photos", "key", source)
            .tags(tags)
            .build()
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidArgument, "case {case}");
        assert_eq!(error.mutation_outcome(), None, "case {case}");
        assert!(error.source().is_some(), "case {case}");
        assert_eq!(poll_count.load(Ordering::Relaxed), 0, "case {case}");
        assert_eq!(drop_count.load(Ordering::Relaxed), 1, "case {case}");
    }
}

#[test]
fn put_build_and_validation_failure_never_poll_or_leak_the_owned_source() {
    for (bucket, digest, expected_error) in [
        ("photos", "1B2M2Y8AsgTpgAmY7PhCfg==", None),
        (
            "PHOTOS",
            "1B2M2Y8AsgTpgAmY7PhCfg==",
            Some(ErrorCode::InvalidBucketName),
        ),
        ("photos", "malformed", Some(ErrorCode::InvalidArgument)),
    ] {
        let poll_count = Arc::new(AtomicUsize::new(0));
        let drop_count = Arc::new(AtomicUsize::new(0));

        let source = BodySource::from_reader(ObservedSource {
            poll_count: Arc::clone(&poll_count),
            drop_count: Arc::clone(&drop_count),
        });

        let result = PutObjectInput::builder(bucket, "key", source)
            .content_md5(digest)
            .build();

        assert_eq!(poll_count.load(Ordering::Relaxed), 0, "{bucket}/{digest}");

        if let Some(code) = expected_error {
            assert_eq!(result.unwrap_err().code(), code, "{bucket}/{digest}");
            assert_eq!(drop_count.load(Ordering::Relaxed), 1, "{bucket}/{digest}");
            continue;
        }

        let input = result.unwrap();
        assert_eq!(drop_count.load(Ordering::Relaxed), 0, "{bucket}/{digest}");
        drop(input);
        assert_eq!(drop_count.load(Ordering::Relaxed), 1, "{bucket}/{digest}");
    }
}

#[test]
fn put_preserves_binary_bytes_literal_identity_absence_and_checked_values() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    let expiry = Timestamp::from_unix_millis(-1).unwrap();

    let mut input = PutObjectInput::builder("photos", "a/../é//%2F", vec![0, 255, 128])
        .expected_bucket_owner("001234567890")
        .content_length(3)
        .metadata(vec![("TITLE".into(), "日本語 é e\u{301}".into())])
        .content_type("")
        .content_disposition("attachment; filename=\"literal.txt\"")
        .expires(expiry)
        .if_none_match("*")
        .build()
        .unwrap();

    assert_eq!(input.bucket(), "photos");
    assert_eq!(input.key(), "a/../é//%2F");
    assert_eq!(input.expected_bucket_owner(), Some("001234567890"));
    assert_eq!(input.content_length(), Some(3));
    assert_eq!(input.metadata()["title"], "日本語 é e\u{301}");
    assert_eq!(input.content_type(), Some(""));
    assert_eq!(input.content_encoding(), None);
    assert_eq!(input.expires(), Some(expiry));
    assert_eq!(input.if_none_match(), Some("*"));
    assert_eq!(input.checksum_algorithm(), None);

    runtime.block_on(async {
        let mut bytes = [0; 2];
        input.body_mut().read_exact(&mut bytes).await?;
        assert_eq!(bytes, [0, 255]);
        let mut source = input.into_body();
        assert_eq!(source.read(&mut bytes).await?, 1);
        assert_eq!(bytes[0], 128);
        assert_eq!(source.read(&mut bytes).await?, 0);
        io::Result::Ok(())
    })?;

    let absent = PutObjectInput::builder("photos", "key", Vec::new())
        .build()
        .unwrap();

    assert_eq!(absent.content_length(), None);
    assert_eq!(absent.content_type(), None);
    assert_eq!(absent.expires(), None);
    assert_eq!(absent.metadata().len(), 0);

    Ok(())
}

#[test]
fn put_shared_limits_and_checksum_conflicts_fail_before_payload_consumption() {
    let builder = || PutObjectInput::builder("photos", "key", Vec::new());

    let failures = [
        builder().content_length(u64::MAX).build(),
        builder().content_length(5_368_709_121).build(),
        builder()
            .metadata(vec![("UP".into(), "1".into()), ("up".into(), "2".into())])
            .build(),
        builder()
            .metadata(
                (0..65)
                    .map(|index| (format!("key{index}"), String::new()))
                    .collect(),
            )
            .build(),
        builder()
            .metadata(vec![("key".into(), "x".repeat(2046))])
            .build(),
        builder()
            .metadata(vec![("key".into(), "\u{85}".into())])
            .build(),
        builder().content_type("text/plain\r\nx-extra: yes").build(),
        builder().content_encoding("é").build(),
        builder().cache_control("x".repeat(2048)).build(),
        builder().expected_bucket_owner("123").build(),
        builder().if_match("unquoted").build(),
        builder().if_none_match("\"specific\"").build(),
        builder().content_md5("AAAAAA==").build(),
        builder().checksum_crc32("AAAAAB==").build(),
        builder()
            .checksum_crc32("AAAAAA==")
            .checksum_sha1("2jmj7l5rSw0yVb/vlWAYkK/YBwk=")
            .build(),
        builder()
            .checksum_algorithm(ChecksumAlgorithm::Sha256)
            .checksum_crc32("AAAAAA==")
            .build(),
    ];

    for (index, result) in failures.into_iter().enumerate() {
        let error = result.unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument, "case {index}");
        assert!(error.source().is_some(), "case {index}");
        assert_eq!(error.mutation_outcome(), None, "case {index}");
    }

    let at_limits = builder()
        .content_length(5_368_709_120)
        .metadata(
            (0..64)
                .map(|index| (format!("key{index}"), String::new()))
                .collect(),
        )
        .content_type("x".repeat(2036))
        .build()
        .unwrap();

    assert_eq!(at_limits.content_length(), Some(5_368_709_120));
    assert_eq!(at_limits.metadata().len(), 64);
}
