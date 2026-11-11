use super::*;

#[test]
fn all_selector_subsets_preserve_public_presence_and_detached_composite_facts() -> TestResult {
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
            .put_object(PutObjectInput::builder("photos", "ordinary", b"abc".to_vec()).build()?)
            .await?;

        let upload = store
            .create_multipart_upload(
                CreateMultipartUploadInput::builder("photos", "composite")
                    .checksum_algorithm(ChecksumAlgorithm::Sha256)
                    .checksum_type(ChecksumType::Composite)
                    .build()?,
            )
            .await?;

        let part = store
            .upload_part(
                UploadPartInput::builder(
                    "photos",
                    "composite",
                    upload.upload_id(),
                    1,
                    b"abc".to_vec(),
                )
                .build()?,
            )
            .await?;

        store
            .complete_multipart_upload(
                CompleteMultipartUploadInput::builder(
                    "photos",
                    "composite",
                    upload.upload_id(),
                    vec![
                        CompletedPart::builder(1, part.etag())
                            .checksum_sha256("ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=")
                            .build()?,
                    ],
                )
                .build()?,
            )
            .await?;

        let groups = [
            ObjectAttribute::Etag,
            ObjectAttribute::Checksum,
            ObjectAttribute::ObjectParts,
            ObjectAttribute::StorageClass,
            ObjectAttribute::ObjectSize,
        ];

        for (key, is_composite, etag) in [
            ("ordinary", false, "\"900150983cd24fb0d6963f7d28e17f72\""),
            ("composite", true, "\"af5da9f45af7a300e3aded972f8ff687-1\""),
        ] {
            for mask in 1..32 {
                let selection = groups
                    .iter()
                    .enumerate()
                    .filter_map(|(index, group)| (mask & (1 << index) != 0).then_some(*group))
                    .collect();

                let output = store
                    .get_object_attributes(
                        GetObjectAttributesInput::builder("photos", key, selection)
                            .max_parts(1)
                            .build()?,
                    )
                    .await?;

                assert_eq!(
                    output.etag(),
                    (mask & 1 != 0).then_some(etag),
                    "{key}/{mask}"
                );

                assert_eq!(output.checksum().is_some(), mask & 2 != 0, "{key}/{mask}");

                assert_eq!(
                    output.object_parts().is_some(),
                    is_composite && mask & 4 != 0,
                    "{key}/{mask}"
                );

                assert_eq!(
                    output.storage_class(),
                    (mask & 8 != 0).then_some("STANDARD"),
                    "{key}/{mask}"
                );

                assert_eq!(
                    output.object_size(),
                    (mask & 16 != 0).then_some(3),
                    "{key}/{mask}"
                );

                if let Some(checksum) = output.checksum() {
                    assert_eq!(
                        checksum.checksum_type(),
                        Some(if is_composite {
                            ChecksumType::Composite
                        } else {
                            ChecksumType::FullObject
                        }),
                        "{key}/{mask}"
                    );

                    if is_composite {
                        assert_eq!(
                            checksum.checksum_sha256(),
                            Some("T4tCwi3TcptRm6b2jS2nzFstYG0F2u1a1RKMwD5sY1g="),
                            "{key}/{mask}"
                        );
                    } else {
                        assert_eq!(
                            checksum.checksum_crc64_nvme(),
                            Some("BeXKuz/B+us="),
                            "{key}/{mask}"
                        );
                    }
                }

                if let Some(parts) = output.object_parts() {
                    assert_eq!(parts.total_parts_count(), 1, "{key}/{mask}");
                    assert_eq!(parts.parts().len(), 1, "{key}/{mask}");
                    assert_eq!(parts.parts()[0].part_number(), 1, "{key}/{mask}");
                    assert_eq!(parts.parts()[0].size(), 3, "{key}/{mask}");

                    assert_eq!(
                        parts.parts()[0].checksum_sha256(),
                        Some("ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="),
                        "{key}/{mask}"
                    );

                    assert!(!parts.is_truncated(), "{key}/{mask}");
                    assert_eq!(parts.next_part_number_marker(), None, "{key}/{mask}");
                }
            }
        }

        let detached = store
            .get_object_attributes(attributes("composite").build()?)
            .await?;

        let exhausted = store
            .get_object_attributes(attributes("composite").part_number_marker(1).build()?)
            .await?;

        assert_eq!(exhausted.object_parts().unwrap().parts(), []);
        assert!(!exhausted.object_parts().unwrap().is_truncated());
        store.close().await?;
        assert_eq!(detached.object_parts().unwrap().parts()[0].size(), 3);
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
