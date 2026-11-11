use super::*;
use crate::access::{HttpIdentity, PrincipalName};
use serde_json::json;

#[test]
fn attributes_conservative_grants_use_current_tags_and_preserve_missing_key_disclosure()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.create_bucket(request().bucket).await?;
        let mut put = super::super::super::super::objects::put(request().key.as_str());
        put.tags = Some(ObjectTags::new([("team".into(), "allowed".into())])?);
        connection.put_object(put, b"abc".as_slice()).await?;

        let principal = connection
            .create_principal(PrincipalName::new("attribute-reader".into())?)
            .await?;

        let key = connection.create_access_key(principal.principal_id).await?;

        let identity = Arc::new(
            connection
                .resolve_application_key(key.access_key.access_key_id.as_str().into())
                .await?
                .verified(),
        );

        for (actions, deny, allowed) in [
            (vec!["s3:ListBucket"], None, false),
            (vec!["s3:GetObject"], None, false),
            (vec!["s3:GetObjectAttributes"], None, false),
            (vec!["s3:GetObject", "s3:GetObjectAttributes"], None, true),
            (
                vec!["s3:GetObject", "s3:GetObjectAttributes"],
                Some("s3:GetObject"),
                false,
            ),
            (
                vec!["s3:GetObject", "s3:GetObjectAttributes"],
                Some("s3:GetObjectAttributes"),
                false,
            ),
        ] {
            let mut statements = vec![json!({
                "Effect":"Allow", "Action":actions, "Resource":"*",
                "Condition":{"StringEquals":{"s3:ExistingObjectTag/team":"allowed"}},
            })];

            if let Some(action) = deny {
                statements.push(json!({"Effect":"Deny", "Action":action, "Resource":"*"}));
            }

            let document = json!({"Version":"2012-10-17", "Statement":statements});

            connection
                .put_principal_policy(principal.principal_id, document.to_string())
                .await?;

            let result = connection
                .get_object_attributes_http(Arc::clone(&identity), attributes())
                .await;

            if allowed {
                assert_eq!(result?.object_size.unwrap().get(), 3);
            } else {
                assert_eq!(
                    result.unwrap_err().failure(),
                    OperationFailure::AccessDenied
                );
            }
        }

        let document = json!({
            "Version":"2012-10-17",
            "Statement":{
                "Effect":"Allow", "Action":["s3:GetObject","s3:GetObjectAttributes"],
                "Resource":"*",
                "Condition":{"StringEquals":{"s3:ExistingObjectTag/team":"allowed"}},
            },
        });

        connection
            .put_principal_policy(principal.principal_id, document.to_string())
            .await?;

        connection
            .replace_object_tags(
                super::super::super::super::objects::read(request().key.as_str()),
                ObjectTags::new([("team".into(), "changed".into())])?,
            )
            .await?;

        let error = connection
            .get_object_attributes_http(Arc::clone(&identity), attributes())
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::AccessDenied);

        for list in [false, true] {
            let mut actions = vec!["s3:GetObject", "s3:GetObjectAttributes"];

            if list {
                actions.push("s3:ListBucket");
            }

            let document = json!({
                "Version":"2012-10-17",
                "Statement":{"Effect":"Allow", "Action":actions,"Resource":"*"},
            });

            connection
                .put_principal_policy(principal.principal_id, document.to_string())
                .await?;

            let mut missing = attributes();
            missing.key = ObjectKey::parse("missing")?;

            let error = connection
                .get_object_attributes_http(Arc::clone(&identity), missing)
                .await
                .unwrap_err();

            assert_eq!(
                error.failure(),
                if list {
                    OperationFailure::NoSuchKey
                } else {
                    OperationFailure::AccessDenied
                }
            );
        }

        let owner = connection.get_owner_info().await?;
        let mut wrong_owner = attributes();

        wrong_owner.expected_owner = Some(
            if owner.account_id() == "000000000000" {
                "111111111111"
            } else {
                "000000000000"
            }
            .into(),
        );

        let error = connection
            .get_object_attributes(wrong_owner.clone())
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::AccessDenied);

        let error = connection
            .get_object_attributes_http(Arc::clone(&identity), wrong_owner)
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::AccessDenied);

        let error = connection
            .get_object_attributes_http(Arc::new(HttpIdentity::anonymous()), attributes())
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::AccessDenied);

        connection
            .delete_access_key(principal.principal_id, key.access_key.access_key_id)
            .await?;

        let error = connection
            .get_object_attributes_http(identity, attributes())
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::AccessDenied);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
