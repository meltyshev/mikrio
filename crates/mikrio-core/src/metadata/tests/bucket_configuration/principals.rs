//! New documents resolve principals in the mutation snapshot; old references remain inert.

use super::*;
use crate::metadata::admin;

fn policy(arn: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":{{"Effect":"Allow","Principal":{{"AWS":"{arn}"}},"Action":"s3:GetBucketAcl","Resource":"arn:aws:s3:::photos"}}}}"#
    )
}

fn stored(database: &MetadataDatabase) -> Result<(Vec<u8>, String), rusqlite::Error> {
    database.connection.query_row(
        "SELECT policy_generation, policy_json FROM buckets WHERE name = 'photos'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

#[test]
fn repeated_policy_principals_are_resolved_once_per_identity_in_the_mutation_snapshot() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let request = request();
    let owner = database.get_owner_info()?;
    let first = database.create_principal(PrincipalName::new("first".into())?)?;
    let second = database.create_principal(PrincipalName::new("second".into())?)?;
    let first_arn = first.principal_id.arn(owner.account_id())?;
    let second_arn = second.principal_id.arn(owner.account_id())?;

    let references = [first_arn.as_str(), second_arn.as_str()]
        .into_iter()
        .cycle()
        .take(64)
        .collect::<Vec<_>>();

    let statement = serde_json::json!({
        "Effect": "Allow",
        "Principal": { "AWS": references },
        "Action": "s3:GetBucketAcl",
        "Resource": "arn:aws:s3:::photos",
    });

    let document = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [statement.clone(), statement],
    })
    .to_string();

    database.connection.flush_prepared_statement_cache();
    database.put_bucket_policy(&Authority::Native, &request, &document)?;
    assert_eq!(admin::principal_lookups(&database.connection)?, 2);

    assert_eq!(
        database
            .get_bucket_policy(&Authority::Native, &request)?
            .document,
        document
    );

    // A repeated historical reference still needs fresh resolution on each apply.
    let before = stored(&database)?;
    database.delete_principal(second.principal_id)?;
    database.connection.flush_prepared_statement_cache();

    let error = database
        .put_bucket_policy(&Authority::Native, &request, &document)
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::InvalidArgument);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
    assert_eq!(stored(&database)?, before);
    assert_eq!(admin::principal_lookups(&database.connection)?, 2);
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn new_bucket_policies_reject_missing_principals_without_invalidating_historical_references()
-> TestResult {
    for native in [true, false] {
        let directory = TestDirectory::new()?.retain_on_failure();
        let mut database = initialized(&directory)?;
        let request = request();
        let owner = database.get_owner_info()?;
        let bootstrap = Bootstrap::generate()?;

        let authority = if native {
            Authority::Native
        } else {
            Authority::Http(Arc::new(
                bootstrap
                    .resolve(bootstrap.access_key_id())
                    .unwrap()
                    .verified(),
            ))
        };

        let original = database.create_principal(PrincipalName::new("original".into())?)?;
        let retained = database.create_principal(PrincipalName::new("retained".into())?)?;
        let original_arn = original.principal_id.arn(owner.account_id())?;
        let retained_arn = retained.principal_id.arn(owner.account_id())?;
        let document = policy(&original_arn);
        database.put_bucket_policy(&authority, &request, &document)?;
        let before = stored(&database)?;

        database.delete_principal(original.principal_id)?;
        let replacement = database.create_principal(original.name)?;
        assert_ne!(replacement.principal_id, original.principal_id);
        let key = database.create_access_key(replacement.principal_id)?;

        let replacement_authority = Authority::Http(Arc::new(
            database
                .resolve_application_key(key.access_key.access_key_id.as_str())?
                .verified(),
        ));

        assert_eq!(
            database
                .get_bucket_acl(&replacement_authority, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::AccessDenied
        );

        assert_eq!(
            database.get_bucket_policy(&authority, &request)?.document,
            document
        );

        assert!(!database.get_bucket_policy_status(&Authority::Native, &request)?);

        for (case_index, candidate) in [
            document.clone(),
            format!(
                r#"{{"Version":"2012-10-17","Statement":{{"Effect":"Allow","Principal":{{"AWS":["{retained_arn}","{original_arn}"]}},"Action":"s3:GetBucketAcl","Resource":"arn:aws:s3:::photos"}}}}"#
            ),
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{retained_arn}"}},"Action":"s3:GetBucketAcl","Resource":"arn:aws:s3:::photos"}},{{"Effect":"Deny","Principal":{{"AWS":"{original_arn}"}},"Action":"s3:GetBucketAcl","Resource":"arn:aws:s3:::photos"}}]}}"#
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let error = database
                .put_bucket_policy(&authority, &request, &candidate)
                .unwrap_err();

            assert_eq!(
                error.failure(),
                OperationFailure::InvalidArgument,
                "native={native}, case {case_index}"
            );

            assert_eq!(
                error.outcome(),
                Some(MutationOutcome::NotCommitted),
                "native={native}, case {case_index}"
            );

            assert_eq!(stored(&database)?, before, "native={native}, case {case_index}");
        }

        close(database)?;

        let mut reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::Existing,
        )?;

        assert_eq!(stored(&reopened)?, before);

        assert_eq!(
            reopened.get_bucket_policy(&authority, &request)?.document,
            document
        );

        // Disabled identities still exist and can be prepared for re-enablement.
        reopened.set_principal_enabled(retained.principal_id, false)?;
        let current = policy(&retained_arn);
        reopened.put_bucket_policy(&authority, &request, &current)?;

        assert_eq!(
            reopened.get_bucket_policy(&authority, &request)?.document,
            current
        );

        assert_ne!(stored(&reopened)?.0, before.0);
        close(reopened)?;
        directory.finish()?;
    }

    Ok(())
}
