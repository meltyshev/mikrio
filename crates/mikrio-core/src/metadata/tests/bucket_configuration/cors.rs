use super::*;
use crate::cors::{CorsConfiguration, CorsRule};

fn rules(origin: &str) -> CorsConfiguration {
    CorsConfiguration::new(vec![CorsRule {
        id: None,
        allowed_origins: vec![origin.into()],
        allowed_methods: vec!["GET".into()],
        allowed_headers: Some(vec![]),
        expose_headers: Some(vec!["ETag".into()]),
        max_age_seconds: Some(0),
    }])
    .unwrap()
}

#[test]
fn cors_replacement_and_deletion_preserve_real_commit_outcomes_and_reopen() -> TestResult {
    use super::super::super::transaction::Fault;

    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let request = request();
    let native = Authority::Native;
    let before = rules("https://before.example");
    let after = rules("https://after.example");

    assert_eq!(
        database
            .get_bucket_cors(&native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchCorsConfiguration
    );

    assert_eq!(database.cors_for_response(&request.bucket)?, None);

    let mut peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    for (fault, outcome, committed) in [
        (Fault::BeforeCommit, MutationOutcome::NotCommitted, false),
        (Fault::AfterCommit, MutationOutcome::Unknown, true),
    ] {
        database.put_bucket_cors(&native, &request, &before)?;
        database.transaction_fault = Some(fault);

        let error = database
            .put_bucket_cors(&native, &request, &after)
            .unwrap_err();

        assert_eq!(error.outcome(), Some(outcome));

        assert_eq!(
            peer.get_bucket_cors(&native, &request)?,
            if committed {
                after.clone()
            } else {
                before.clone()
            }
        );

        database.transaction_fault = Some(fault);
        let error = database.delete_bucket_cors(&native, &request).unwrap_err();
        assert_eq!(error.outcome(), Some(outcome));

        assert_eq!(
            peer.cors_for_response(&request.bucket)?,
            if committed {
                None
            } else {
                Some(before.clone())
            }
        );
    }

    database.put_bucket_cors(&native, &request, &after)?;
    close(peer)?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(reopened.get_bucket_cors(&native, &request)?, after);
    reopened.delete_bucket_cors(&native, &request)?;
    reopened.delete_bucket_cors(&native, &request)?;

    assert_eq!(
        reopened.get_public_access_block(&native, &request)?,
        flags(15)
    );

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn cors_public_matching_does_not_grant_configuration_authority_and_corruption_fails_closed()
-> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let request = request();
    let configuration = rules("*");
    database.put_bucket_cors(&Authority::Native, &request, &configuration)?;
    let anonymous = Authority::Http(Arc::new(crate::access::HttpIdentity::anonymous()));

    assert_eq!(
        database.cors_for_response(&request.bucket)?,
        Some(configuration.clone())
    );

    for (operation_index, result) in [
        database.get_bucket_cors(&anonymous, &request).map(drop),
        database.put_bucket_cors(&anonymous, &request, &configuration),
        database.delete_bucket_cors(&anonymous, &request),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            result.unwrap_err().failure(),
            OperationFailure::AccessDenied,
            "operation {operation_index}"
        );
    }

    let owner = database.get_owner_info()?;

    let wrong_owner = if owner.account_id() == "000000000000" {
        "111111111111"
    } else {
        "000000000000"
    };

    let wrong = BucketAccessRequest {
        expected_owner: Some(wrong_owner.into()),
        ..request.clone()
    };

    assert_eq!(
        database
            .get_bucket_cors(&Authority::Native, &wrong)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    for encoded in [
        "{}",
        "[]",
        r#"[{"allowed_origins":["*"],"allowed_methods":["OPTIONS"]}]"#,
    ] {
        database.connection.execute(
            "UPDATE buckets SET cors_rules_json = ?1 WHERE name = 'photos'",
            [encoded],
        )?;

        assert_eq!(
            database
                .cors_for_response(&request.bucket)
                .unwrap_err()
                .failure(),
            OperationFailure::StoreCorrupt
        );

        assert_eq!(
            database
                .get_bucket_cors(&Authority::Native, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::StoreCorrupt
        );
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}
