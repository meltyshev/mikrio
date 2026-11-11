use mikrio::*;

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn public_copy_and_multipart_share_durable_objects_and_preserve_failed_replacements() -> TestResult
{
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

        let source = store
            .put_object(
                PutObjectInput::builder("photos", "literal/%2f+source", b"abc".to_vec())
                    .metadata(vec![("origin".into(), "source".into())])
                    .tags(vec![Tag {
                        key: "team".into(),
                        value: "source".into(),
                    }])
                    .build()?,
            )
            .await?;

        let copied = store
            .copy_object(
                CopyObjectInput::builder(
                    "photos",
                    "copy",
                    ObjectRef::new("photos", "literal/%2f+source")?,
                )
                .copy_source_if_match(source.etag())
                .if_none_match("*")
                .build()?,
            )
            .await?;

        assert_eq!(copied.etag(), source.etag());

        let head = store
            .head_object(HeadObjectInput::builder("photos", "copy").build()?)
            .await?;

        assert_eq!(head.metadata()["origin"], "source");
        assert_eq!(head.tag_count(), 1);

        let error = store
            .copy_object(
                CopyObjectInput::builder(
                    "photos",
                    "copy",
                    ObjectRef::new("photos", "literal/%2f+source")?,
                )
                .if_none_match("*")
                .build()?,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::PreconditionFailed);

        assert_eq!(
            error.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        let created = store
            .create_multipart_upload(
                CreateMultipartUploadInput::builder("photos", "assembled")
                    .metadata(vec![("origin".into(), "initiation".into())])
                    .build()?,
            )
            .await?;

        assert_eq!(created.checksum_algorithm(), ChecksumAlgorithm::Crc64Nvme);
        assert_eq!(created.checksum_type(), ChecksumType::FullObject);

        let uploads = store
            .list_multipart_uploads(ListMultipartUploadsInput::builder("photos").build()?)
            .await?;

        assert_eq!(uploads.uploads().len(), 1);
        assert_eq!(uploads.uploads()[0].upload_id(), created.upload_id());

        let part = store
            .upload_part(
                UploadPartInput::builder(
                    "photos",
                    "assembled",
                    created.upload_id(),
                    7,
                    b"before".to_vec(),
                )
                .build()?,
            )
            .await?;

        let error = store
            .upload_part(
                UploadPartInput::builder(
                    "photos",
                    "assembled",
                    created.upload_id(),
                    7,
                    b"bad".to_vec(),
                )
                .content_length(4)
                .build()?,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::IncompleteBody);

        assert_eq!(
            error.mutation_outcome(),
            Some(&MutationOutcome::NotCommitted)
        );

        let listed = store
            .list_parts(
                ListPartsInput::builder("photos", "assembled", created.upload_id())
                    .max_parts(1)
                    .build()?,
            )
            .await?;

        assert_eq!(listed.parts()[0].etag(), part.etag());

        let copied_part = store
            .upload_part_copy(
                UploadPartCopyInput::builder(
                    "photos",
                    "assembled",
                    created.upload_id(),
                    7,
                    ObjectRef::new("photos", "copy")?,
                )
                .build()?,
            )
            .await?;

        store
            .delete_object(DeleteObjectInput::builder("photos", "copy").build()?)
            .await?;

        let completed = store
            .complete_multipart_upload(
                CompleteMultipartUploadInput::builder(
                    "photos",
                    "assembled",
                    created.upload_id(),
                    vec![CompletedPart::builder(7, copied_part.etag()).build()?],
                )
                .content_length(3)
                .checksum_type(ChecksumType::FullObject)
                .build()?,
            )
            .await?;

        assert!(completed.etag().ends_with("-1\""));
        assert_eq!(completed.checksum_crc64_nvme(), Some("BeXKuz/B+us="));

        assert_eq!(
            store
                .list_parts(
                    ListPartsInput::builder("photos", "assembled", created.upload_id()).build()?
                )
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoSuchUpload
        );

        let abandoned = store
            .create_multipart_upload(
                CreateMultipartUploadInput::builder("photos", "abandoned").build()?,
            )
            .await?;

        store
            .abort_multipart_upload(
                AbortMultipartUploadInput::builder("photos", "abandoned", abandoned.upload_id())
                    .build()?,
            )
            .await?;

        assert_eq!(
            store
                .list_multipart_uploads(ListMultipartUploadsInput::builder("photos").build()?)
                .await?
                .uploads(),
            []
        );

        store.close().await?;
        let reopened = stores.open(OpenOptions::new(&path)).await?;

        let mut read = reopened
            .get_object(GetObjectInput::builder("photos", "assembled").build()?)
            .await?;

        assert_eq!(read.metadata()["origin"], "initiation");
        assert_eq!(read.body_mut().read_all(3).await?, b"abc");
        read.body_mut().close().await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;

    Ok(())
}

#[test]
fn ranged_copy_verifies_and_completes_the_selected_bytes() -> TestResult {
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

        let mut source = vec![b'x'; 5 * 1024 * 1024 + 1];
        source[65535..65538].copy_from_slice(b"abc");

        store
            .put_object(PutObjectInput::builder("photos", "source", source).build()?)
            .await?;

        let created = store
            .create_multipart_upload(
                CreateMultipartUploadInput::builder("photos", "ranged")
                    .checksum_algorithm(ChecksumAlgorithm::Sha256)
                    .build()?,
            )
            .await?;

        let part = store
            .upload_part_copy(
                UploadPartCopyInput::builder(
                    "photos",
                    "ranged",
                    created.upload_id(),
                    1,
                    ObjectRef::new("photos", "source")?,
                )
                .copy_source_range("bytes=65535-65537")
                .build()?,
            )
            .await?;

        assert_eq!(part.etag(), "\"900150983cd24fb0d6963f7d28e17f72\"");

        assert_eq!(
            part.checksum_sha256(),
            Some("ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=")
        );

        store
            .complete_multipart_upload(
                CompleteMultipartUploadInput::builder(
                    "photos",
                    "ranged",
                    created.upload_id(),
                    vec![
                        CompletedPart::builder(1, part.etag())
                            .checksum_sha256(part.checksum_sha256().unwrap())
                            .build()?,
                    ],
                )
                .build()?,
            )
            .await?;

        let mut object = store
            .get_object(GetObjectInput::builder("photos", "ranged").build()?)
            .await?;

        assert_eq!(object.body_mut().read_all(3).await?, b"abc");
        object.body_mut().close().await?;
        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;

    Ok(())
}
