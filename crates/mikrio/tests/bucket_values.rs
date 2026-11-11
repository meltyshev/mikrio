use std::error::Error as _;

use mikrio::{
    CreateBucketInput, DeleteBucketInput, ErrorCode, GetBucketLocationInput, HeadBucketInput,
};

#[test]
fn every_bucket_input_uses_literal_shared_names_and_typed_errors() {
    for name in ["photos", "a.b", &"a".repeat(63)] {
        assert_eq!(
            CreateBucketInput::builder(name).build().unwrap().bucket(),
            name
        );

        assert_eq!(
            HeadBucketInput::builder(name).build().unwrap().bucket(),
            name
        );

        assert_eq!(
            DeleteBucketInput::builder(name).build().unwrap().bucket(),
            name
        );

        assert_eq!(
            GetBucketLocationInput::builder(name)
                .build()
                .unwrap()
                .bucket(),
            name
        );
    }

    for name in [
        "",
        "UPPERCASE",
        "two..dots",
        "127.0.0.1",
        "café",
        &"a".repeat(64),
    ] {
        for (operation, error) in [
            (
                "create",
                CreateBucketInput::builder(name).build().unwrap_err(),
            ),
            ("head", HeadBucketInput::builder(name).build().unwrap_err()),
            (
                "delete",
                DeleteBucketInput::builder(name).build().unwrap_err(),
            ),
            (
                "location",
                GetBucketLocationInput::builder(name).build().unwrap_err(),
            ),
        ] {
            assert_eq!(
                error.code(),
                ErrorCode::InvalidBucketName,
                "{operation}: {name:?}"
            );

            assert_eq!(
                error.code().as_str(),
                "InvalidBucketName",
                "{operation}: {name:?}"
            );

            assert!(error.source().is_some(), "{operation}: {name:?}");
        }
    }
}

#[test]
fn owner_assertions_preserve_leading_zeroes_and_reject_noncanonical_accounts() {
    let account = "001234567890";

    let head = HeadBucketInput::builder("photos")
        .expected_bucket_owner(account)
        .build()
        .unwrap();

    let delete = DeleteBucketInput::builder("photos")
        .expected_bucket_owner(account)
        .build()
        .unwrap();

    let location = GetBucketLocationInput::builder("photos")
        .expected_bucket_owner(account)
        .build()
        .unwrap();

    assert_eq!(head.expected_bucket_owner(), Some(account));
    assert_eq!(delete.expected_bucket_owner(), Some(account));
    assert_eq!(location.expected_bucket_owner(), Some(account));

    assert_eq!(
        HeadBucketInput::builder("photos")
            .build()
            .unwrap()
            .expected_bucket_owner(),
        None
    );

    for account in [
        "",
        "12345678901",
        "1234567890123",
        "１２３４５６７８９０１２",
        "12345678901\n",
    ] {
        for (operation, error) in ["head", "delete", "location"].into_iter().zip([
            HeadBucketInput::builder("photos")
                .expected_bucket_owner(account)
                .build()
                .unwrap_err(),
            DeleteBucketInput::builder("photos")
                .expected_bucket_owner(account)
                .build()
                .unwrap_err(),
            GetBucketLocationInput::builder("photos")
                .expected_bucket_owner(account)
                .build()
                .unwrap_err(),
        ]) {
            assert_eq!(
                error.code(),
                ErrorCode::InvalidArgument,
                "{operation}: {account:?}"
            );

            assert!(error.source().is_some(), "{operation}: {account:?}");
        }
    }
}
