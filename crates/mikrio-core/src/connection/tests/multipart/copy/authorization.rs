use super::*;
use crate::access::{HttpIdentity, PrincipalName};
use crate::object::ReadConditionsInput;
use serde_json::json;

#[test]
fn part_copy_checks_both_resources_without_inventing_tag_write_or_final_creation_context()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.create_bucket(request().bucket).await?;
        let mut source = super::super::super::objects::put("source");
        source.tags = Some(ObjectTags::new([("team".into(), "source".into())])?);
        connection.put_object(source, b"abc".as_slice()).await?;
        let created = connection.create_multipart_upload(request()).await?;

        connection
            .upload_part(
                super::super::parts::part(created.upload_id.clone(), 1),
                b"before".as_slice(),
            )
            .await?;

        let principal = connection
            .create_principal(PrincipalName::new("part-copier".into())?)
            .await?;

        let key = connection.create_access_key(principal.principal_id).await?;

        let identity = Arc::new(
            connection
                .resolve_application_key(key.access_key.access_key_id.as_str().into())
                .await?
                .verified(),
        );

        for (read, write, request_tags, allowed) in [
            (true, false, false, false),
            (false, true, false, false),
            (true, true, false, true),
            (true, true, true, false),
        ] {
            let mut statements = vec![json!({
                "Effect":"Deny", "Action":["s3:GetObjectTagging","s3:PutObjectTagging"],
                "Resource":"arn:aws:s3:::attachments/*",
            })];

            if read {
                statements.push(json!({
                    "Effect":"Allow", "Action":"s3:GetObject",
                    "Resource":"arn:aws:s3:::attachments/source",
                    "Condition":{"StringEquals":{"s3:ExistingObjectTag/team":"source"}},
                }));
            }

            if write {
                let condition = if request_tags {
                    json!({"StringEquals":{"s3:RequestObjectTag/team":"source"}})
                } else {
                    json!({"Bool":{"s3:ObjectCreationOperation":"false"}})
                };

                statements.push(json!({
                    "Effect":"Allow", "Action":"s3:PutObject",
                    "Resource":format!("arn:aws:s3:::attachments/{}", request().key.as_str()),
                    "Condition":condition,
                }));
            }

            connection
                .put_principal_policy(
                    principal.principal_id,
                    json!({
                        "Version":"2012-10-17", "Statement":statements,
                    })
                    .to_string(),
                )
                .await?;

            let previous = page(&connection, created.upload_id.clone()).await?.parts;

            let result = connection
                .upload_part_copy_http(Arc::clone(&identity), input(created.upload_id.clone(), 1))
                .await;

            if allowed {
                assert_eq!(result?.facts.etag(), "\"900150983cd24fb0d6963f7d28e17f72\"");
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.failure(), OperationFailure::AccessDenied);
                assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

                assert_eq!(
                    page(&connection, created.upload_id.clone()).await?.parts,
                    previous
                );
            }
        }

        let mut conditional = input(created.upload_id.clone(), 1);

        conditional.source_conditions = ReadConditions::new(ReadConditionsInput {
            if_none_match: Some("\"900150983cd24fb0d6963f7d28e17f72\"".into()),
            ..ReadConditionsInput::default()
        })?;

        let error = connection.upload_part_copy(conditional).await.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::PreconditionFailed);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

        let error = connection
            .upload_part_copy_http(
                Arc::new(HttpIdentity::anonymous()),
                input(created.upload_id.clone(), 1),
            )
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::AccessDenied);
        complete(&connection, created.upload_id).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
