use mikrio::{
    ChecksumAlgorithm, ChecksumMode, CreateBucketInput, DeleteObjectInput, ErrorCode,
    GetObjectInput, HeadObjectInput, OpenOptions, PutObjectInput, Tag,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

#[test]
fn persisted_object_facade_preserves_metadata_checksums_conditions_and_binary_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("attachments").build()?)
            .await?;

        let bytes = vec![0, 255, 3, 10];

        let result = store
            .put_object(
                PutObjectInput::builder("attachments", "literal/%2f/../file", bytes.clone())
                    .content_length(4)
                    .content_type("")
                    .content_encoding("identity")
                    .metadata(vec![("Mixed-Case".into(), "exact value".into())])
                    .tags(vec![Tag {
                        key: "Status".into(),
                        value: "Ready".into(),
                    }])
                    .checksum_algorithm(ChecksumAlgorithm::Sha256)
                    .build()?,
            )
            .await?;

        let etag = result.etag().to_owned();
        let checksum = result.checksum_sha256().unwrap().to_owned();

        let head = store
            .head_object(HeadObjectInput::builder("attachments", "literal/%2f/../file").build()?)
            .await?;

        assert_eq!(head.content_length(), 4);
        assert_eq!(head.etag(), etag);
        assert_eq!(head.content_type(), Some(""));
        assert_eq!(head.tag_count(), 1);
        assert_eq!(head.metadata()["mixed-case"], "exact value");
        assert_eq!(head.checksum_sha256(), None);

        assert_eq!(
            store
                .head_object(
                    HeadObjectInput::builder("attachments", "literal/%2f/../file")
                        .if_none_match(format!("W/{etag}"))
                        .build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NotModified
        );

        store.close().await?;
        let reopened = stores.open(OpenOptions::new(path)).await?;

        let mut output = reopened
            .get_object(
                GetObjectInput::builder("attachments", "literal/%2f/../file")
                    .checksum_mode(ChecksumMode::Enabled)
                    .if_match(etag)
                    .build()?,
            )
            .await?;

        assert_eq!(output.checksum_sha256(), Some(checksum.as_str()));

        reopened
            .delete_object(
                DeleteObjectInput::builder("attachments", "literal/%2f/../file").build()?,
            )
            .await?;

        assert_eq!(output.body_mut().read_all(4).await?, bytes);
        assert_eq!(output.body_mut().read_chunk(1).await?, None);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn empty_put_has_verified_facts_default_content_type_and_zero_length_eof()
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

        let result = store
            .put_object(PutObjectInput::builder("attachments", "empty", Vec::<u8>::new()).build()?)
            .await?;

        assert_eq!(result.etag(), "\"d41d8cd98f00b204e9800998ecf8427e\"");
        assert_eq!(result.checksum_crc64_nvme(), Some("AAAAAAAAAAA="));

        let mut output = store
            .get_object(GetObjectInput::builder("attachments", "empty").build()?)
            .await?;

        assert_eq!(output.content_type(), Some("binary/octet-stream"));
        assert_eq!(output.content_length(), 0);
        assert_eq!(output.body_mut().read_all(0).await?, Vec::<u8>::new());
        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
