use super::*;
use crate::access::{HttpIdentity, PrincipalName};
use serde_json::json;

pub(super) fn policy(source: bool, destination: &[&str], condition: serde_json::Value) -> String {
    let mut target = json!({
        "Effect": "Allow",
        "Action": destination,
        "Resource": "arn:aws:s3:::attachments/destination",
    });

    if condition.as_object().is_some_and(|value| !value.is_empty()) {
        target["Condition"] = condition;
    }

    let mut statements = vec![target];

    if source {
        statements.push(json!({
            "Effect": "Allow",
            "Action": "s3:GetObject",
            "Resource": "arn:aws:s3:::attachments/source",
            "Condition": {"StringEquals": {"s3:ExistingObjectTag/team": "source"}},
        }));
    }

    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

#[test]
fn copy_requires_separate_source_destination_and_tag_grants_with_operation_specific_context()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.create_bucket(request().bucket).await?;
        let mut source = objects::put("source");
        source.tags = Some(ObjectTags::new([("team".into(), "source".into())])?);
        connection.put_object(source, b"abc".as_slice()).await?;

        let principal = connection
            .create_principal(PrincipalName::new("copy-worker".into())?)
            .await?;

        let key = connection.create_access_key(principal.principal_id).await?;

        let identity = Arc::new(
            connection
                .resolve_application_key(key.access_key.access_key_id.as_str().into())
                .await?
                .verified(),
        );

        for (source, destination, condition, replace, allowed) in [
            (
                false,
                vec!["s3:PutObject", "s3:PutObjectTagging"],
                json!({}),
                false,
                false,
            ),
            (true, vec!["s3:PutObject"], json!({}), false, false),
            (
                true,
                vec!["s3:PutObject", "s3:PutObjectTagging"],
                json!({}),
                false,
                true,
            ),
            (
                true,
                vec!["s3:PutObject", "s3:PutObjectTagging"],
                json!({"StringEquals": {"s3:RequestObjectTag/team": "source"}}),
                false,
                false,
            ),
            (
                true,
                vec!["s3:PutObject", "s3:PutObjectTagging"],
                json!({"StringEquals": {"s3:ExistingObjectTag/team": "source"}}),
                false,
                false,
            ),
            (
                true,
                vec!["s3:PutObject", "s3:PutObjectTagging"],
                json!({"StringEquals": {"s3:RequestObjectTag/team": "source"}}),
                true,
                true,
            ),
        ] {
            connection
                .put_principal_policy(
                    principal.principal_id,
                    policy(source, &destination, condition),
                )
                .await?;

            let mut input = request();

            if replace {
                input.tagging_directive = CopyDirective::Replace;
                input.tags = Some(ObjectTags::new([("team".into(), "source".into())])?);
            }

            let result = connection
                .copy_object_http(Arc::clone(&identity), input)
                .await;

            if allowed {
                result?;
                assert_eq!(bytes(&connection, "destination").await?, b"abc");
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.failure(), OperationFailure::AccessDenied);
                assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
            }
        }

        // COPY does not need GetObjectTagging on the source. If-Match adds
        // destination read authority independently of the source read grant.
        let previous = connection.head_object(objects::read("destination")).await?;
        let mut conditional = request();
        conditional.conditions = WriteConditions::new(Some(previous.etag), None)?;

        for allowed in [false, true] {
            let mut actions = vec!["s3:PutObject", "s3:PutObjectTagging"];

            if allowed {
                actions.push("s3:GetObject");
            }

            connection
                .put_principal_policy(principal.principal_id, policy(true, &actions, json!({})))
                .await?;

            let result = connection
                .copy_object_http(Arc::clone(&identity), conditional.clone())
                .await;

            if allowed {
                result?;
            } else {
                assert_eq!(
                    result.unwrap_err().failure(),
                    OperationFailure::AccessDenied
                );
            }
        }

        let error = connection
            .copy_object_http(Arc::new(HttpIdentity::anonymous()), request())
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::AccessDenied);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
