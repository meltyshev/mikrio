use mikrio::{
    CreateBucketInput, DeleteBucketInput, ErrorCode, GetBucketLocationInput, HeadBucketInput,
    MutationOutcome, OpenOptions,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

#[test]
fn buckets_persist_across_independent_connections_close_and_reopen()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let first = stores.open(OpenOptions::new(&path)).await?;
        let second = stores.open(OpenOptions::new(&path)).await?;

        first
            .create_bucket(CreateBucketInput::builder("attachments").build()?)
            .await?;

        second
            .create_bucket(CreateBucketInput::builder("attachments").build()?)
            .await?;

        assert_eq!(
            second
                .head_bucket(HeadBucketInput::builder("attachments").build()?)
                .await?
                .bucket_region(),
            "us-east-1"
        );

        assert_eq!(
            second
                .get_bucket_location(GetBucketLocationInput::builder("attachments").build()?)
                .await?
                .location_constraint(),
            None
        );

        first.close().await?;

        let error = first
            .delete_bucket(DeleteBucketInput::builder("attachments").build()?)
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::StoreClosed);

        assert_eq!(
            error.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        second.close().await?;

        let reopened = stores.open(OpenOptions::new(&path)).await?;

        reopened
            .head_bucket(HeadBucketInput::builder("attachments").build()?)
            .await?;

        reopened
            .delete_bucket(DeleteBucketInput::builder("attachments").build()?)
            .await?;

        assert_eq!(
            reopened
                .head_bucket(HeadBucketInput::builder("attachments").build()?)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchBucket
        );

        let error = reopened
            .delete_bucket(DeleteBucketInput::builder("attachments").build()?)
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::NoSuchBucket);

        assert_eq!(
            error.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn nondefault_region_repeated_create_reports_a_typed_uncommitted_conflict()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")).region("eu-west-1"))
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("attachments").build()?)
            .await?;

        let error = store
            .create_bucket(CreateBucketInput::builder("attachments").build()?)
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::BucketAlreadyOwnedByYou);

        assert_eq!(
            error.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        assert_eq!(
            store
                .get_bucket_location(GetBucketLocationInput::builder("attachments").build()?)
                .await?
                .location_constraint(),
            Some("eu-west-1")
        );

        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
