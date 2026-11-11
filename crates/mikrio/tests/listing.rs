use mikrio::{
    CreateBucketInput, DeleteBucketInput, DeleteObjectInput, ErrorCode, ListBucketsInput,
    ListObjectsV2Input, OpenOptions, PutObjectInput,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

#[test]
fn bucket_pages_survive_reopen_with_live_cursors_and_detached_facts()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;

        assert_eq!(
            store
                .list_buckets(ListBucketsInput::builder().build()?)
                .await?
                .buckets(),
            []
        );

        for bucket in ["aaa", "bbb", "ccc"] {
            store
                .create_bucket(CreateBucketInput::builder(bucket).build()?)
                .await?;
        }

        let first = store
            .list_buckets(ListBucketsInput::builder().max_buckets(1).build()?)
            .await?;

        assert_eq!(first.buckets()[0].name(), "aaa");
        assert_eq!(first.buckets()[0].bucket_region(), "us-east-1");
        let token = first.continuation_token().unwrap().to_owned();
        store.close().await?;
        let store = stores.open(OpenOptions::new(&path)).await?;

        store
            .delete_bucket(DeleteBucketInput::builder("bbb").build()?)
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("ddd").build()?)
            .await?;

        let last = store
            .list_buckets(
                ListBucketsInput::builder()
                    .max_buckets(2)
                    .continuation_token(&token)
                    .build()?,
            )
            .await?;

        assert_eq!(
            last.buckets()
                .iter()
                .map(mikrio::BucketSummary::name)
                .collect::<Vec<_>>(),
            ["ccc", "ddd"]
        );

        assert!(last.continuation_token().is_none());

        assert_eq!(
            store
                .list_buckets(
                    ListBucketsInput::builder()
                        .prefix("")
                        .continuation_token(&token)
                        .build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument
        );

        store.close().await?;
        assert_eq!(first.buckets()[0].name(), "aaa");
        assert!(first.buckets()[0].creation_date().as_unix_millis() > 0);

        assert_eq!(
            store
                .list_buckets(ListBucketsInput::builder().build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::StoreClosed
        );

        let other = stores
            .open(OpenOptions::new(directory.path().join("other")))
            .await?;

        assert_eq!(
            other
                .list_buckets(
                    ListBucketsInput::builder()
                        .continuation_token(token)
                        .build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument
        );

        other.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn listing_pages_survive_reopen_and_remain_live_without_repeating_groups()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        for key in ["a", "b/one", "b/two", "c"] {
            store
                .put_object(PutObjectInput::builder("photos", key, b"abc".to_vec()).build()?)
                .await?;
        }

        let first = store
            .list_objects_v2(
                ListObjectsV2Input::builder("photos")
                    .delimiter("/")
                    .max_keys(2)
                    .build()?,
            )
            .await?;

        assert_eq!(first.contents()[0].key(), "a");
        assert_eq!(first.contents()[0].size(), 3);

        assert_eq!(
            first.contents()[0].etag(),
            "\"900150983cd24fb0d6963f7d28e17f72\""
        );

        assert_eq!(first.common_prefixes()[0].prefix(), "b/");
        assert!(first.is_truncated());
        let token = first.next_continuation_token().unwrap().to_owned();
        store.close().await?;

        let store = stores.open(OpenOptions::new(&path)).await?;

        store
            .delete_object(DeleteObjectInput::builder("photos", "c").build()?)
            .await?;

        for key in ["b/late", "d"] {
            store
                .put_object(PutObjectInput::builder("photos", key, vec![]).build()?)
                .await?;
        }

        let next = store
            .list_objects_v2(
                ListObjectsV2Input::builder("photos")
                    .delimiter("/")
                    .continuation_token(&token)
                    .start_after("z")
                    .max_keys(1)
                    .build()?,
            )
            .await?;

        assert_eq!(next.contents()[0].key(), "d");
        assert_eq!(next.common_prefixes(), []);
        assert!(!next.is_truncated());
        assert!(next.next_continuation_token().is_none());

        for key in ["a", "b/one", "b/two", "b/late", "d"] {
            store
                .delete_object(DeleteObjectInput::builder("photos", key).build()?)
                .await?;
        }

        store
            .delete_bucket(DeleteBucketInput::builder("photos").build()?)
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        assert_eq!(
            store
                .list_objects_v2(
                    ListObjectsV2Input::builder("photos")
                        .delimiter("/")
                        .continuation_token(&token)
                        .build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument
        );

        store.close().await?;

        assert_eq!(
            store
                .list_objects_v2(ListObjectsV2Input::builder("photos").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::StoreClosed
        );

        let other = stores
            .open(OpenOptions::new(directory.path().join("other")))
            .await?;

        other
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        assert_eq!(
            other
                .list_objects_v2(
                    ListObjectsV2Input::builder("photos")
                        .delimiter("/")
                        .continuation_token(token)
                        .build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument
        );

        other.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
