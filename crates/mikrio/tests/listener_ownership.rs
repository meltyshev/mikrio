#![cfg(feature = "http")]

use mikrio::{CreateBucketInput, ErrorCode, HeadBucketInput, ListenOptions, OpenOptions};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

#[test]
fn listener_close_and_rejected_second_listener_preserve_direct_store_use()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = test_directory::TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        let server = store.listen(ListenOptions::default()).await?;
        let endpoint = server.endpoint().to_owned();
        assert!(endpoint.starts_with("http://127.0.0.1:"));
        assert_eq!(server.region(), "us-east-1");

        let error = store
            .listen(ListenOptions::default())
            .await
            .err()
            .ok_or("second listener accepted")?;

        assert_eq!(error.code(), ErrorCode::ListenerAlreadyRunning);
        assert_eq!(error.mutation_outcome(), None);

        let connection =
            tokio::net::TcpStream::connect(endpoint.trim_start_matches("http://")).await?;

        drop(connection);
        server.close().await?;
        server.close().await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        assert_eq!(
            store
                .head_bucket(HeadBucketInput::builder("photos").build()?)
                .await?
                .bucket_region(),
            "us-east-1"
        );

        let replacement = store.listen(ListenOptions::default()).await?;
        store.close().await?;
        replacement.close().await?;

        assert_eq!(
            store
                .listen(ListenOptions::default())
                .await
                .err()
                .ok_or("closed store listened")?
                .code(),
            ErrorCode::StoreClosed
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
