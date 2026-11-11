use std::error::Error as _;

use mikrio::{
    ChecksumMode, DeleteObjectInput, ErrorCode, GetObjectInput, HeadObjectInput, ObjectRef,
    Timestamp,
};

#[test]
fn object_identities_own_exact_keys_and_share_validation_with_delete() {
    for key in ["a//b/", "a/../b", "percent%2Fslash", "é", "e\u{301}"] {
        let owned_key = key.to_owned();
        let reference = ObjectRef::new("photos", &owned_key).unwrap();

        let delete = DeleteObjectInput::builder("photos", owned_key)
            .build()
            .unwrap();

        let get = GetObjectInput::builder("photos", key)
            .expected_bucket_owner("001234567890")
            .checksum_mode(ChecksumMode::Enabled)
            .build()
            .unwrap();

        let head = HeadObjectInput::builder("photos", key).build().unwrap();

        assert_eq!(reference.bucket(), "photos", "{key:?}");
        assert_eq!(reference.key().as_bytes(), key.as_bytes());
        assert_eq!(delete.bucket(), reference.bucket(), "{key:?}");
        assert_eq!(delete.key(), reference.key());
        assert_eq!(delete.expected_bucket_owner(), None, "{key:?}");
        assert_eq!(delete.if_match(), None, "{key:?}");
        assert_eq!(get.key(), key);
        assert_eq!(head.key(), key);
        assert_eq!(get.expected_bucket_owner(), Some("001234567890"), "{key:?}");
        assert_eq!(get.checksum_mode(), Some(ChecksumMode::Enabled), "{key:?}");
        assert_eq!(head.expected_bucket_owner(), None, "{key:?}");
        assert_eq!(head.checksum_mode(), None, "{key:?}");
    }

    for (bucket, key, expected) in [
        ("PHOTOS", "valid", ErrorCode::InvalidBucketName),
        ("photos", "", ErrorCode::InvalidArgument),
        ("photos", "../escape", ErrorCode::InvalidArgument),
        ("photos", &"é".repeat(513), ErrorCode::InvalidArgument),
    ] {
        for (operation, failure) in ["reference", "delete", "get", "head"].into_iter().zip([
            ObjectRef::new(bucket, key).unwrap_err(),
            DeleteObjectInput::builder(bucket, key).build().unwrap_err(),
            GetObjectInput::builder(bucket, key).build().unwrap_err(),
            HeadObjectInput::builder(bucket, key).build().unwrap_err(),
        ]) {
            assert_eq!(failure.code(), expected, "{operation}: {bucket:?}/{key:?}");

            assert!(
                failure.source().is_some(),
                "{operation}: {bucket:?}/{key:?}"
            );
        }
    }
}

#[test]
fn read_owner_assertions_reject_noncanonical_accounts_without_defaulting_them() {
    for account in [
        "",
        "12345678901",
        "1234567890123",
        "１２３４５６７８９０１２",
    ] {
        for (operation, failure) in ["get", "head"].into_iter().zip([
            GetObjectInput::builder("photos", "key")
                .expected_bucket_owner(account)
                .build()
                .unwrap_err(),
            HeadObjectInput::builder("photos", "key")
                .expected_bucket_owner(account)
                .build()
                .unwrap_err(),
        ]) {
            assert_eq!(
                failure.code(),
                ErrorCode::InvalidArgument,
                "{operation}: {account:?}"
            );

            assert!(failure.source().is_some(), "{operation}: {account:?}");
        }
    }
}

#[test]
fn read_conditions_preserve_entity_tag_syntax_and_reject_injection() {
    for condition in [
        "*",
        "\"opaque\"",
        "W/\"weak\"",
        "\"has,comma\", W/\"é\"",
        " , \"tag\",, ",
    ] {
        let get = GetObjectInput::builder("photos", "key")
            .if_match(condition)
            .if_none_match(condition)
            .build()
            .unwrap();

        let head = HeadObjectInput::builder("photos", "key")
            .if_match(condition)
            .if_none_match(condition)
            .build()
            .unwrap();

        assert_eq!(get.if_match(), Some(condition));
        assert_eq!(get.if_none_match(), Some(condition));
        assert_eq!(head.if_match(), Some(condition));
        assert_eq!(head.if_none_match(), Some(condition));
    }

    for condition in [
        "",
        " , , ",
        "unquoted",
        "\"missing-end",
        "*, \"tag\"",
        "\"tag\", *",
        "\"first\"\"second\"",
        "\"space inside\"",
        "\"line\r\ninjection\"",
        "\"del\u{7f}\"",
    ] {
        for (condition_name, failure) in [
            "get.if_match",
            "get.if_none_match",
            "head.if_match",
            "head.if_none_match",
        ]
        .into_iter()
        .zip([
            GetObjectInput::builder("photos", "key")
                .if_match(condition)
                .build()
                .unwrap_err(),
            GetObjectInput::builder("photos", "key")
                .if_none_match(condition)
                .build()
                .unwrap_err(),
            HeadObjectInput::builder("photos", "key")
                .if_match(condition)
                .build()
                .unwrap_err(),
            HeadObjectInput::builder("photos", "key")
                .if_none_match(condition)
                .build()
                .unwrap_err(),
        ]) {
            assert_eq!(
                failure.code(),
                ErrorCode::InvalidArgument,
                "{condition_name}: {condition:?}"
            );
        }
    }
}

#[test]
fn read_condition_dates_round_down_to_whole_seconds_across_the_epoch_and_shared_bounds() {
    for (supplied, expected) in [
        (-62_135_596_800_000, -62_135_596_800_000),
        (-1001, -2000),
        (-1000, -1000),
        (-999, -1000),
        (-1, -1000),
        (0, 0),
        (1, 0),
        (999, 0),
        (1000, 1000),
        (253_402_300_799_999, 253_402_300_799_000),
    ] {
        let instant = Timestamp::from_unix_millis(supplied).unwrap();

        let get = GetObjectInput::builder("photos", "key")
            .if_modified_since(instant)
            .if_unmodified_since(instant)
            .build()
            .unwrap();

        let head = HeadObjectInput::builder("photos", "key")
            .if_modified_since(instant)
            .if_unmodified_since(instant)
            .build()
            .unwrap();

        for (condition_name, normalized) in [
            "get.if_modified_since",
            "get.if_unmodified_since",
            "head.if_modified_since",
            "head.if_unmodified_since",
        ]
        .into_iter()
        .zip([
            get.if_modified_since(),
            get.if_unmodified_since(),
            head.if_modified_since(),
            head.if_unmodified_since(),
        ]) {
            assert_eq!(
                normalized.unwrap().as_unix_millis(),
                expected,
                "{condition_name}: {supplied}"
            );
        }
    }

    let absent = GetObjectInput::builder("photos", "key").build().unwrap();
    assert_eq!(absent.if_match(), None);
    assert_eq!(absent.if_none_match(), None);
    assert_eq!(absent.if_modified_since(), None);
    assert_eq!(absent.if_unmodified_since(), None);
}

#[test]
fn delete_conditions_preserve_opaque_etags_and_reject_injection_or_multiple_values() {
    for condition in [
        "*",
        "\"\"",
        "\"opaque,not-a-digest\"",
        "\"back\\slash\"",
        "\"é\"",
    ] {
        let input = DeleteObjectInput::builder("photos", "key")
            .expected_bucket_owner("001234567890")
            .if_match(condition)
            .build()
            .unwrap();

        assert_eq!(input.if_match(), Some(condition));

        assert_eq!(
            input.expected_bucket_owner(),
            Some("001234567890"),
            "{condition:?}"
        );
    }

    for condition in [
        "",
        "unquoted",
        " * ",
        "W/\"weak\"",
        "\"first\", \"second\"",
        "\"embedded\"quote\"",
        "\"has space\"",
        "\"has\ttab\"",
        "\"line\r\ninjection\"",
        "\"nul\0\"",
        "\"del\u{7f}\"",
    ] {
        let failure = DeleteObjectInput::builder("photos", "key")
            .if_match(condition)
            .build()
            .unwrap_err();

        assert_eq!(failure.code(), ErrorCode::InvalidArgument, "{condition:?}");
        assert!(failure.source().is_some(), "{condition:?}");
    }

    let failure = DeleteObjectInput::builder("photos", "key")
        .expected_bucket_owner("１２３４５６７８９０１２")
        .build()
        .unwrap_err();

    assert_eq!(failure.code(), ErrorCode::InvalidArgument);
}
