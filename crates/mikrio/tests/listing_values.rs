use mikrio::{ErrorCode, ListBucketsInput, ListObjectsV2Input};

#[test]
fn listing_filters_preserve_literal_boundaries_without_object_key_normalization() {
    for prefix in [
        "",
        "../",
        "a//b/./",
        "literal%2F",
        "é",
        "e\u{301}",
        &"x".repeat(1025),
    ] {
        let objects = ListObjectsV2Input::builder("photos")
            .prefix(prefix)
            .delimiter("//")
            .start_after("../not-an-existing-key")
            .continuation_token("opaque%2F+//=")
            .expected_bucket_owner("001234567890")
            .build()
            .unwrap();

        let buckets = ListBucketsInput::builder()
            .prefix(prefix)
            .continuation_token("")
            .build()
            .unwrap();

        assert_eq!(objects.prefix(), Some(prefix), "prefix={prefix:?}");
        assert_eq!(objects.delimiter(), Some("//"), "prefix={prefix:?}");

        assert_eq!(
            objects.start_after(),
            Some("../not-an-existing-key"),
            "prefix={prefix:?}"
        );

        assert_eq!(
            objects.continuation_token(),
            Some("opaque%2F+//="),
            "prefix={prefix:?}"
        );

        assert_eq!(
            objects.expected_bucket_owner(),
            Some("001234567890"),
            "prefix={prefix:?}"
        );

        assert_eq!(buckets.prefix(), Some(prefix), "prefix={prefix:?}");
        assert_eq!(buckets.continuation_token(), Some(""), "prefix={prefix:?}");
    }

    let absent = ListObjectsV2Input::builder("photos").build().unwrap();
    assert_eq!(absent.prefix(), None);
    assert_eq!(absent.delimiter(), None);
    assert_eq!(absent.start_after(), None);
    assert_eq!(absent.continuation_token(), None);
}

#[test]
fn listing_page_bounds_are_operation_specific_and_never_clamp_supplied_values() {
    assert_eq!(
        ListObjectsV2Input::builder("photos")
            .build()
            .unwrap()
            .max_keys(),
        1000
    );

    assert_eq!(
        ListBucketsInput::builder().build().unwrap().max_buckets(),
        1000
    );

    for count in [0, 1, 1000] {
        assert_eq!(
            ListObjectsV2Input::builder("photos")
                .max_keys(count)
                .build()
                .unwrap()
                .max_keys(),
            count
        );
    }

    for count in [1, 1000, 1001, 10_000] {
        assert_eq!(
            ListBucketsInput::builder()
                .max_buckets(count)
                .build()
                .unwrap()
                .max_buckets(),
            count
        );
    }

    for count in [1001, 10_000, u32::MAX] {
        assert_eq!(
            ListObjectsV2Input::builder("photos")
                .max_keys(count)
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument,
            "count={count}"
        );
    }

    for count in [0, 10_001, u32::MAX] {
        assert_eq!(
            ListBucketsInput::builder()
                .max_buckets(count)
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument,
            "count={count}"
        );
    }
}

#[test]
fn object_listing_uses_the_shared_bucket_and_owner_rules() {
    assert_eq!(
        ListObjectsV2Input::builder("PHOTOS")
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidBucketName
    );

    for owner in ["", "123", "１２３４５６７８９０１２"] {
        let error = ListObjectsV2Input::builder("photos")
            .expected_bucket_owner(owner)
            .build()
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidArgument, "owner={owner:?}");
        assert_eq!(error.mutation_outcome(), None, "owner={owner:?}");
    }
}
