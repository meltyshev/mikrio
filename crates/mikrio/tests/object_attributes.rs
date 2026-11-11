use mikrio::*;

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

#[path = "object_attributes/matrix.rs"]
mod matrix;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const MIN_PART_BYTES: usize = 5 * 1024 * 1024;

fn attributes(key: &str) -> GetObjectAttributesInputBuilder {
    GetObjectAttributesInput::builder(
        "photos",
        key,
        vec![
            ObjectAttribute::Etag,
            ObjectAttribute::Checksum,
            ObjectAttribute::ObjectParts,
            ObjectAttribute::ObjectSize,
            ObjectAttribute::StorageClass,
        ],
    )
}

#[test]
fn public_attributes_and_part_reads_preserve_mode_identity_and_cursor_ownership() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        let mut source = vec![b'a'; MIN_PART_BYTES];
        source.extend_from_slice(b"tail");

        store
            .put_object(PutObjectInput::builder("photos", "source", source).build()?)
            .await?;

        let original = store
            .get_object_attributes(attributes("source").build()?)
            .await?;

        assert!(original.object_parts().is_none());

        assert_eq!(
            original.object_size(),
            Some(u64::try_from(MIN_PART_BYTES + 4)?)
        );

        assert_eq!(original.storage_class(), Some("STANDARD"));

        for (key, is_composite) in [("full", false), ("composite", true)] {
            let mut create = CreateMultipartUploadInput::builder("photos", key);

            if is_composite {
                create = create
                    .checksum_algorithm(ChecksumAlgorithm::Sha256)
                    .checksum_type(ChecksumType::Composite);
            }

            let upload = store.create_multipart_upload(create.build()?).await?;
            let (first_number, last_number) = if is_composite { (1, 2) } else { (2, 7) };

            let first = store
                .upload_part_copy(
                    UploadPartCopyInput::builder(
                        "photos",
                        key,
                        upload.upload_id(),
                        first_number,
                        ObjectRef::new("photos", "source")?,
                    )
                    .copy_source_range("bytes=0-5242879")
                    .build()?,
                )
                .await?;

            let last = store
                .upload_part(
                    UploadPartInput::builder(
                        "photos",
                        key,
                        upload.upload_id(),
                        last_number,
                        b"tail".to_vec(),
                    )
                    .build()?,
                )
                .await?;

            let mut first_completed = CompletedPart::builder(first_number, first.etag());
            let mut last_completed = CompletedPart::builder(last_number, last.etag());

            if is_composite {
                first_completed = first_completed.checksum_sha256(first.checksum_sha256().unwrap());

                last_completed = last_completed.checksum_sha256(last.checksum_sha256().unwrap());
            }

            let completed = store
                .complete_multipart_upload(
                    CompleteMultipartUploadInput::builder(
                        "photos",
                        key,
                        upload.upload_id(),
                        vec![first_completed.build()?, last_completed.build()?],
                    )
                    .build()?,
                )
                .await?;

            let page = store
                .get_object_attributes(attributes(key).max_parts(1).build()?)
                .await?;

            assert_eq!(page.etag(), Some(completed.etag()), "{key}");

            assert_eq!(
                page.object_size(),
                Some(u64::try_from(MIN_PART_BYTES + 4)?),
                "{key}"
            );

            let parts = page.object_parts().unwrap();
            assert_eq!(parts.total_parts_count(), 2, "{key}");
            assert_eq!(parts.max_parts(), 1, "{key}");
            assert_eq!(parts.parts().len(), usize::from(is_composite), "{key}");
            assert_eq!(parts.is_truncated(), is_composite, "{key}");

            if is_composite {
                assert_eq!(parts.next_part_number_marker(), Some(1), "{key}");

                assert_eq!(
                    parts.parts()[0].size(),
                    u64::try_from(MIN_PART_BYTES)?,
                    "{key}"
                );

                assert_eq!(
                    parts.parts()[0].checksum_sha256(),
                    first.checksum_sha256(),
                    "{key}"
                );

                assert!(
                    !page
                        .checksum()
                        .unwrap()
                        .checksum_sha256()
                        .unwrap()
                        .contains('-'),
                    "{key}"
                );

                let tail = store
                    .get_object_attributes(
                        attributes(key).max_parts(1).part_number_marker(1).build()?,
                    )
                    .await?;

                let tail = tail.object_parts().unwrap();
                assert!(!tail.is_truncated(), "{key}");
                assert_eq!(tail.next_part_number_marker(), None, "{key}");
                assert_eq!(tail.parts()[0].size(), 4, "{key}");
            }

            let head = store
                .head_object(
                    HeadObjectInput::builder("photos", key)
                        .part_number(2)
                        .checksum_mode(ChecksumMode::Enabled)
                        .build()?,
                )
                .await?;

            assert_eq!(head.content_length(), 4, "{key}");

            assert_eq!(
                head.content_range(),
                Some("bytes 5242880-5242883/5242884"),
                "{key}"
            );

            assert_eq!(head.parts_count(), Some(2), "{key}");
            assert_eq!(head.etag(), completed.etag(), "{key}");

            assert_eq!(
                head.checksum_type(),
                is_composite.then_some(ChecksumType::Composite),
                "{key}"
            );

            if is_composite {
                assert_eq!(head.checksum_sha256(), last.checksum_sha256(), "{key}");
            }

            copy_across_completed_boundary(&store, key, completed.etag()).await?;

            let mut cursor = store
                .get_object(
                    GetObjectInput::builder("photos", key)
                        .part_number(2)
                        .build()?,
                )
                .await?;

            store
                .put_object(
                    PutObjectInput::builder("photos", key, b"replacement".to_vec()).build()?,
                )
                .await?;

            let replacement = store
                .get_object_attributes(attributes(key).part_number_marker(1).build()?)
                .await?;

            assert!(replacement.object_parts().is_none(), "{key}");
            assert_eq!(replacement.object_size(), Some(11), "{key}");
            assert_eq!(page.object_parts().unwrap().total_parts_count(), 2, "{key}");

            assert_eq!(cursor.body_mut().read_all(4).await?, b"tail", "{key}");
            cursor.body_mut().close().await?;

            let missing = store
                .head_object(
                    HeadObjectInput::builder("photos", key)
                        .part_number(2)
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(missing.code(), ErrorCode::InvalidPartNumber, "{key}");

            let conflict = store
                .get_object(
                    GetObjectInput::builder("photos", key)
                        .part_number(1)
                        .range("bytes=0-0")
                        .build()?,
                )
                .await
                .unwrap_err();

            assert_eq!(conflict.code(), ErrorCode::InvalidRequest, "{key}");
        }

        store.close().await?;
        assert_eq!(original.storage_class(), Some("STANDARD"));
        let reopened = stores.open(OpenOptions::new(&path)).await?;

        let mut source = reopened
            .get_object(
                GetObjectInput::builder("photos", "source")
                    .part_number(1)
                    .build()?,
            )
            .await?;

        assert_eq!(source.parts_count(), None);
        assert_eq!(source.body_mut().read_chunk(1).await?.unwrap(), b"a");
        source.body_mut().close().await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

async fn copy_across_completed_boundary(store: &Store, source: &str, etag: &str) -> TestResult {
    let key = format!("slice-{source}");

    let upload = store
        .create_multipart_upload(
            CreateMultipartUploadInput::builder("photos", &key)
                .checksum_algorithm(ChecksumAlgorithm::Sha256)
                .checksum_type(ChecksumType::Composite)
                .build()?,
        )
        .await?;

    let copied = store
        .upload_part_copy(
            UploadPartCopyInput::builder(
                "photos",
                &key,
                upload.upload_id(),
                1,
                ObjectRef::new("photos", source)?,
            )
            .copy_source_range("bytes=5242879-5242883")
            .copy_source_if_match(etag)
            .build()?,
        )
        .await?;

    // Independent SHA-256 of the five selected bytes, not either source digest.
    assert_eq!(
        copied.checksum_sha256(),
        Some("zmj7yPpi45Kke1gOlL+2NurWl4B8b193T4oAdf9NArQ=")
    );

    store
        .complete_multipart_upload(
            CompleteMultipartUploadInput::builder(
                "photos",
                &key,
                upload.upload_id(),
                vec![
                    CompletedPart::builder(1, copied.etag())
                        .checksum_sha256(copied.checksum_sha256().ok_or("missing copied checksum")?)
                        .build()?,
                ],
            )
            .build()?,
        )
        .await?;

    let mut selected = store
        .get_object(
            GetObjectInput::builder("photos", &key)
                .part_number(1)
                .build()?,
        )
        .await?;

    assert_eq!(selected.body_mut().read_all(5).await?, b"atail");
    selected.body_mut().close().await?;
    Ok(())
}

#[test]
fn empty_object_and_empty_completed_part_return_empty_owned_cursors() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.path().join("store")))
            .await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_object(PutObjectInput::builder("photos", "ordinary", Vec::new()).build()?)
            .await?;

        let upload = store
            .create_multipart_upload(
                CreateMultipartUploadInput::builder("photos", "multipart").build()?,
            )
            .await?;

        let part = store
            .upload_part(
                UploadPartInput::builder("photos", "multipart", upload.upload_id(), 1, Vec::new())
                    .build()?,
            )
            .await?;

        store
            .complete_multipart_upload(
                CompleteMultipartUploadInput::builder(
                    "photos",
                    "multipart",
                    upload.upload_id(),
                    vec![CompletedPart::builder(1, part.etag()).build()?],
                )
                .build()?,
            )
            .await?;

        for (key, count) in [("ordinary", None), ("multipart", Some(1))] {
            let mut selected = store
                .get_object(
                    GetObjectInput::builder("photos", key)
                        .part_number(1)
                        .build()?,
                )
                .await?;

            assert_eq!(selected.content_length(), 0, "{key}");
            assert_eq!(selected.content_range(), None, "{key}");
            assert_eq!(selected.parts_count(), count, "{key}");
            assert_eq!(selected.body_mut().read_all(0).await?, b"", "{key}");
            selected.body_mut().close().await?;
        }

        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn attributes_builders_reject_unbounded_or_ambiguous_selection() {
    assert_eq!(
        GetObjectAttributesInput::builder("photos", "key", vec![])
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    assert_eq!(
        GetObjectAttributesInput::builder("photos", "key", vec![ObjectAttribute::Etag; 2])
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    for size in [0, 1001, u32::MAX] {
        assert_eq!(
            attributes("key")
                .max_parts(size)
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument,
            "size={size}"
        );
    }

    assert_eq!(
        attributes("key")
            .part_number_marker(10_001)
            .build()
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    for number in [0, 10_001] {
        assert_eq!(
            GetObjectInput::builder("photos", "key")
                .part_number(number)
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument,
            "part_number={number}"
        );

        assert_eq!(
            HeadObjectInput::builder("photos", "key")
                .part_number(number)
                .build()
                .unwrap_err()
                .code(),
            ErrorCode::InvalidArgument,
            "part_number={number}"
        );
    }
}
