//! Current authority is rechecked after preliminary selection, before a body exists.

use super::objects::{put, read};
use super::*;
use crate::OperationFailure;
use crate::access::PrincipalName;
use crate::object::{BucketName, ObjectTags, ReadConditions, ReadConditionsInput};

const POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::attachments/selected","Condition":{"StringEquals":{"s3:ExistingObjectTag/visibility":"public"}}}}"#;

#[test]
fn final_guarded_read_rechecks_peer_changes_after_preliminary_authorization() -> TestResult {
    guarded_read_rechecks(false)
}

#[test]
fn final_part_read_rechecks_peer_changes_after_preliminary_authorization() -> TestResult {
    guarded_read_rechecks(true)
}

fn guarded_read_rechecks(part: bool) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        for change in ["tags", "replace", "policy", "key", "delete"] {
            let path = directory.path().join(change);
            let reader = Connection::open(path.clone(), None).await?;
            let peer = Connection::open(path, None).await?;

            reader
                .create_bucket(BucketName::parse("attachments")?)
                .await?;

            let mut original = put("selected");
            original.tags = Some(ObjectTags::new([("visibility".into(), "public".into())])?);

            if part {
                seed_parts(&reader, original, b"original bytes").await?;
            } else {
                reader
                    .put_object(original, b"original bytes".as_slice())
                    .await?;
            }

            let predecessor = reader.head_object(read("selected")).await?;

            let principal = reader
                .create_principal(PrincipalName::new("selected-reader".into())?)
                .await?;

            reader
                .put_principal_policy(principal.principal_id, POLICY.into())
                .await?;

            let key = reader.create_access_key(principal.principal_id).await?;

            let identity = Arc::new(
                reader
                    .resolve_application_key(key.access_key.access_key_id.as_str().into())
                    .await?
                    .verified(),
            );

            let (selected, observed) = oneshot::channel();
            let (release, released) = oneshot::channel();

            assert!(
                reader
                    .owner
                    .state
                    .context
                    .test_hooks
                    .after_read_selection
                    .lock()
                    .unwrap()
                    .replace((selected, released))
                    .is_none()
            );

            let pending_owner = reader.clone();
            let pending_identity = Arc::clone(&identity);

            let pending = tokio::spawn(async move {
                let mut request = read("selected");
                request.part_number = part.then(|| crate::multipart::PartNumber::new(2).unwrap());

                pending_owner
                    .get_object_http(pending_identity, request)
                    .await
            });

            tokio::time::timeout(Duration::from_secs(10), observed).await??;

            match change {
                "tags" => {
                    peer.replace_object_tags(
                        read("selected"),
                        ObjectTags::new([("visibility".into(), "private".into())])?,
                    )
                    .await?;
                }
                "replace" => {
                    peer.put_object(put("selected"), b"private replacement".as_slice())
                        .await?;
                }
                "policy" => {
                    peer.put_principal_policy(
                        principal.principal_id,
                        POLICY.replace("\"Allow\"", "\"Deny\""),
                    )
                    .await?;
                }
                "key" => {
                    peer.delete_access_key(principal.principal_id, key.access_key.access_key_id)
                        .await?;
                }
                "delete" => {
                    peer.delete_object(read("selected"), None).await?;
                }
                _ => return Err("unknown read-authorization test case".into()),
            }

            release
                .send(())
                .map_err(|()| "read selection owner ended")?;

            let error = tokio::time::timeout(Duration::from_secs(10), pending)
                .await??
                .err()
                .ok_or("stale preliminary authorization returned a body")?;

            assert_eq!(error.failure(), OperationFailure::AccessDenied, "{change}");

            let mut conditional = read("selected");
            conditional.part_number = part.then(|| crate::multipart::PartNumber::new(2).unwrap());

            conditional.conditions = ReadConditions::new(ReadConditionsInput {
                if_none_match: Some(predecessor.etag),
                ..ReadConditionsInput::default()
            })?;

            let error = reader
                .head_object_http(identity, conditional)
                .await
                .unwrap_err();

            assert_eq!(error.failure(), OperationFailure::AccessDenied, "{change}");

            reader.close().await?;
            peer.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

pub(super) async fn seed_parts(
    connection: &Connection,
    original: crate::object::PutRequest,
    tail: &[u8],
) -> TestResult {
    use crate::integrity::ExpectedDigests;
    use crate::multipart::{
        CompleteRequest, CompletionParts, CreateRequest, MultipartChecksum, PartNumber,
        PartRequest, SelectedPart, UploadRequest,
    };

    let upload = connection
        .create_multipart_upload(CreateRequest {
            bucket: original.bucket.clone(),
            key: original.key.clone(),
            expected_owner: None,
            representation: original.representation,
            metadata: original.metadata,
            tags: original.tags,
            checksum: MultipartChecksum::resolve(None, None)?,
        })
        .await?;

    let mut parts = Vec::new();

    for (number, bytes) in [(1, vec![b'x'; 5 * 1024 * 1024]), (2, tail.to_vec())] {
        let part_number = PartNumber::new(number)?;

        let written = connection
            .upload_part(
                PartRequest {
                    bucket: original.bucket.clone(),
                    key: original.key.clone(),
                    upload_id: upload.upload_id.clone(),
                    part_number,
                    expected_owner: None,
                    content_length: None,
                    checksum_algorithm: None,
                    expected: ExpectedDigests::default(),
                },
                std::io::Cursor::new(bytes),
            )
            .await?;

        parts.push(SelectedPart {
            part_number,
            etag: written.etag().clone(),
            checksum: Some(written.checksum()),
        });
    }

    connection
        .complete_multipart_upload(CompleteRequest {
            upload: UploadRequest {
                bucket: original.bucket,
                key: original.key,
                upload_id: upload.upload_id,
                expected_owner: None,
            },
            parts: CompletionParts::new(parts)?,
            conditions: crate::object::WriteConditions::default(),
            content_length: None,
            checksum_type: None,
            checksum: None,
        })
        .await?;

    Ok(())
}
