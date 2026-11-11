use mikrio::{
    ChecksumMode, CreateBucketInput, ErrorCode, GetObjectInput, HeadObjectInput, OpenOptions,
    PutObjectInput, Timestamp,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

#[test]
fn range_and_response_overrides_are_detached_and_shared_by_get_and_head()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("attachments").build()?)
            .await?;

        store
            .put_object(
                PutObjectInput::builder("attachments", "object", b"testcontent".to_vec())
                    .content_type("text/plain")
                    .build()?,
            )
            .await?;

        let expires = Timestamp::from_unix_millis(1_500_000_000_123)?;

        let input = GetObjectInput::builder("attachments", "object")
            .range("bytes=4-7")
            .checksum_mode(ChecksumMode::Enabled)
            .response_content_type("application/pdf")
            .response_content_encoding("identity")
            .response_content_disposition("attachment")
            .response_content_language("")
            .response_cache_control("no-store")
            .response_expires(expires)
            .build()?;

        assert_eq!(input.range(), Some("bytes=4-7"));
        assert_eq!(input.response_expires(), Some(expires));
        let mut output = store.get_object(input).await?;
        assert_eq!(output.content_length(), 4);
        assert_eq!(output.content_range(), Some("bytes 4-7/11"));
        assert_eq!(output.content_type(), Some("application/pdf"));
        assert_eq!(output.content_encoding(), Some("identity"));
        assert_eq!(output.content_disposition(), Some("attachment"));
        assert_eq!(output.content_language(), Some(""));
        assert_eq!(output.cache_control(), Some("no-store"));
        assert_eq!(output.expires(), Some(expires));
        assert_eq!(output.checksum_crc64_nvme(), None);
        assert_eq!(output.body_mut().read_all(4).await?, b"cont");
        output.body_mut().close().await?;

        let head = store
            .head_object(
                HeadObjectInput::builder("attachments", "object")
                    .range("bytes=-7")
                    .response_content_type("")
                    .response_content_encoding("identity")
                    .response_content_disposition("inline")
                    .response_content_language("fr")
                    .response_cache_control("no-cache")
                    .response_expires(expires)
                    .build()?,
            )
            .await?;

        assert_eq!(head.content_length(), 7);
        assert_eq!(head.content_range(), Some("bytes 4-10/11"));
        assert_eq!(head.content_type(), Some(""));
        assert_eq!(head.content_encoding(), Some("identity"));
        assert_eq!(head.content_disposition(), Some("inline"));
        assert_eq!(head.content_language(), Some("fr"));
        assert_eq!(head.cache_control(), Some("no-cache"));
        assert_eq!(head.expires(), Some(expires));

        let original = store
            .head_object(
                HeadObjectInput::builder("attachments", "object")
                    .checksum_mode(ChecksumMode::Enabled)
                    .build()?,
            )
            .await?;

        assert_eq!(original.content_length(), 11);
        assert_eq!(original.content_type(), Some("text/plain"));
        assert_eq!(original.content_range(), None);
        assert!(original.checksum_crc64_nvme().is_some());
        assert_eq!(original.etag(), head.etag());
        assert_eq!(original.last_modified(), head.last_modified());

        assert_eq!(
            store
                .head_object(
                    HeadObjectInput::builder("attachments", "object")
                        .range("bytes=99-")
                        .build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRange
        );

        assert_eq!(
            GetObjectInput::builder("attachments", "object")
                .response_content_type("bad\r\nheader")
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument
        );

        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
