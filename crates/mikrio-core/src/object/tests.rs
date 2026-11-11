use super::*;

use serde::Deserialize;

#[derive(Deserialize)]
struct Fixtures {
    valid_keys: Vec<String>,
    invalid_keys: Vec<String>,
    valid_buckets: Vec<String>,
    invalid_buckets: Vec<String>,
    metadata: Vec<(String, String)>,
    stored_metadata: String,
    content_types: Vec<ContentTypeFixture>,
}

#[derive(Deserialize)]
struct ContentTypeFixture {
    input: Option<String>,
    effective: String,
}

fn fixtures() -> Fixtures {
    serde_json::from_str(include_str!(
        "../../../../tests/contract/object-values.json"
    ))
    .unwrap()
}

#[test]
fn keys_and_bucket_names_preserve_literals_and_reject_invalid_boundaries() {
    let fixtures = fixtures();

    for key in fixtures.valid_keys {
        let parsed = ObjectKey::parse(&key).unwrap();
        assert_eq!(parsed.as_str(), key);
        assert_eq!(parsed.as_bytes(), key.as_bytes());
        assert_eq!(ObjectKey::from_bytes(parsed.as_bytes()).unwrap(), parsed);
    }

    for key in fixtures.invalid_keys {
        assert_eq!(
            ObjectKey::parse(&key),
            Err(ObjectValueError::KeyRelativeSegments),
            "key {key:?}"
        );
    }

    for bucket in fixtures.valid_buckets {
        assert_eq!(BucketName::parse(&bucket).unwrap().as_str(), bucket);
    }

    for bucket in fixtures.invalid_buckets {
        assert_eq!(
            BucketName::parse(&bucket),
            Err(ObjectValueError::BucketName),
            "bucket {bucket:?}"
        );
    }

    assert_eq!(ObjectKey::parse(""), Err(ObjectValueError::KeyLength));
    assert!(ObjectKey::parse(&"é".repeat(512)).is_ok());

    assert_eq!(
        ObjectKey::parse(&"é".repeat(513)),
        Err(ObjectValueError::KeyLength)
    );

    assert_eq!(
        ObjectKey::from_bytes(&[0xed, 0xa0, 0x80]),
        Err(ObjectValueError::KeyEncoding)
    );

    assert_ne!(ObjectKey::parse("é"), ObjectKey::parse("e\u{301}"));
    assert!(BucketName::parse(&"a".repeat(63)).is_ok());

    assert_eq!(
        BucketName::parse(&"a".repeat(64)),
        Err(ObjectValueError::BucketName)
    );
}

#[test]
fn metadata_round_trips_exact_values_and_rejects_noncanonical_storage() {
    let fixtures = fixtures();
    let metadata = UserMetadata::new(fixtures.metadata).unwrap();
    assert_eq!(metadata.to_json().unwrap(), fixtures.stored_metadata);

    assert_eq!(
        UserMetadata::from_json(&fixtures.stored_metadata).unwrap(),
        metadata
    );

    assert_eq!(UserMetadata::default().to_json().unwrap(), "{}");

    for encoded in [
        r#"{"name":"first","name":"last"}"#,
        r#"{"Name":"value"}"#,
        r#"{"name":null}"#,
        r#"{"name":1}"#,
        r#"{"name":[]}"#,
        r#"{"":"empty key"}"#,
        r#"{"na me":"value"}"#,
        r#"{"name":"\r\ninjected: value"}"#,
        "[]",
        "null",
        "{} {}",
    ] {
        assert_eq!(
            UserMetadata::from_json(encoded),
            Err(ObjectValueError::MetadataEncoding),
            "accepted {encoded:?}"
        );
    }

    assert_eq!(
        UserMetadata::new([
            ("Name".into(), "first".into()),
            ("name".into(), "last".into())
        ]),
        Err(ObjectValueError::MetadataDuplicate)
    );

    for control in ['\0', '\t', '\r', '\n', '\u{7f}', '\u{85}'] {
        assert_eq!(
            UserMetadata::new([("name".into(), format!("before{control}after"))]),
            Err(ObjectValueError::MetadataValue),
            "control={control:?}"
        );
    }

    for key in ["", "na me", "na:me", "café", "name\r\nx-injected"] {
        assert_eq!(
            UserMetadata::new([(key.into(), "value".into())]),
            Err(ObjectValueError::MetadataKey),
            "key={key:?}"
        );
    }
}

#[test]
fn metadata_budgets_count_utf8_and_escaped_json_fits_the_disk_envelope() {
    for value in ["é".repeat(1023), "\\".repeat(2046), "\"".repeat(2046)] {
        let metadata = UserMetadata::new([("id".into(), value.clone())]).unwrap();
        let encoded = metadata.to_json().unwrap();
        assert!(encoded.len() <= 65_536);
        assert_eq!(UserMetadata::from_json(&encoded).unwrap(), metadata);

        assert_eq!(
            UserMetadata::new([("id".into(), format!("{value}x"))]),
            Err(ObjectValueError::MetadataSize),
            "character={:?}",
            value.chars().next()
        );
    }

    let oversized = format!(r#"{{"id":"{}"}}"#, "x".repeat(2047));

    assert_eq!(
        UserMetadata::from_json(&oversized),
        Err(ObjectValueError::MetadataEncoding)
    );
}

#[test]
fn metadata_entry_limit_applies_to_inputs_and_persisted_objects() {
    let entries: Vec<_> = (0..64)
        .map(|index| (format!("key{index:02}"), String::new()))
        .collect();

    let metadata = UserMetadata::new(entries.clone()).unwrap();
    assert_eq!(metadata.entries().len(), 64);

    assert_eq!(
        UserMetadata::from_json(&metadata.to_json().unwrap()).unwrap(),
        metadata
    );

    let mut excessive = entries;
    excessive.push(("extra".into(), String::new()));

    assert_eq!(
        UserMetadata::new(excessive.clone()),
        Err(ObjectValueError::MetadataCount)
    );

    let encoded = serde_json::to_string(
        &excessive
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .unwrap();

    assert_eq!(
        UserMetadata::from_json(&encoded),
        Err(ObjectValueError::MetadataEncoding)
    );
}

#[test]
fn representation_preserves_absence_and_enforces_combined_header_budget() {
    let input = RepresentationInput {
        content_type: Some(String::new()),
        content_encoding: Some("gzip".into()),
        content_disposition: Some("attachment; filename=\"photo.jpg\"".into()),
        content_language: Some("en".into()),
        cache_control: Some("max-age=60".into()),
        expires: Some(EpochMillis::new(-1).unwrap()),
    };

    assert_eq!(Representation::new(input.clone()).unwrap().fields(), &input);
    assert_eq!(Representation::default().fields().content_type, None);

    for value in [
        "text/plain\r\ninjected: yes",
        "text/plain\t",
        "café",
        "\u{7f}",
    ] {
        let invalid = RepresentationInput {
            content_type: Some(value.into()),
            ..RepresentationInput::default()
        };

        assert_eq!(
            Representation::new(invalid),
            Err(ObjectValueError::RepresentationValue),
            "value={value:?}"
        );
    }

    let exact = RepresentationInput {
        content_type: Some("x".repeat(2048 - "content-type".len() - "expires".len() - 29)),
        expires: Some(EpochMillis::new(0).unwrap()),
        ..RepresentationInput::default()
    };

    assert!(Representation::new(exact.clone()).is_ok());

    assert_eq!(
        Representation::new(RepresentationInput {
            cache_control: Some(String::new()),
            ..exact
        }),
        Err(ObjectValueError::RepresentationSize)
    );
}

#[test]
fn content_type_defaults_only_when_absent_and_never_sniffs_payloads() {
    for case in fixtures().content_types {
        let representation = Representation::new(RepresentationInput {
            content_type: case.input.clone(),
            ..RepresentationInput::default()
        })
        .unwrap();

        assert_eq!(representation.fields().content_type, case.input);
        assert_eq!(representation.effective_content_type(), case.effective);
    }
}

#[test]
fn byte_lengths_reject_negative_or_unrepresentable_storage_integers() {
    for length in [0, 1, (1_u64 << 53) + 1, i64::MAX.unsigned_abs()] {
        let checked = ByteLength::new(length).unwrap();
        assert_eq!(checked.get(), length);

        assert_eq!(
            ByteLength::from_stored(checked.as_stored()).unwrap(),
            checked
        );
    }

    assert_eq!(
        ByteLength::from_stored(-1),
        Err(ObjectValueError::ByteLength)
    );

    assert_eq!(
        ByteLength::new(1_u64 << 63),
        Err(ObjectValueError::ByteLength)
    );

    assert_eq!(ByteLength::new(u64::MAX), Err(ObjectValueError::ByteLength));
}
