use super::*;
use crate::lifecycle::{LifecycleConfiguration, LifecycleFilter, LifecycleRule};

fn rules(prefix: &str) -> LifecycleConfiguration {
    LifecycleConfiguration::new(vec![
        LifecycleRule {
            id: Some("expire".into()),
            is_enabled: true,
            filter: LifecycleFilter::Prefix(prefix.into()),
            expiration_days: Some(1),
            abort_incomplete_multipart_days: None,
        },
        LifecycleRule {
            id: Some("abort".into()),
            is_enabled: false,
            filter: LifecycleFilter::LegacyPrefix(String::new()),
            expiration_days: None,
            abort_incomplete_multipart_days: Some(2_147_483_647),
        },
    ])
    .unwrap()
}

fn generation(database: &MetadataDatabase) -> rusqlite::Result<Option<Vec<u8>>> {
    database.connection.query_row(
        "SELECT lifecycle_generation FROM buckets WHERE name = 'photos'",
        [],
        |row| row.get(0),
    )
}

#[test]
fn weakened_lifecycle_constraint_is_refused_without_rewriting_configuration() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let database = initialized(&directory)?;

    let original: String = database.connection.query_row(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'bucket_lifecycle_rules'",
        [],
        |row| row.get(0),
    )?;

    let incompatible = original.replacen(
        "expiration_days BETWEEN 1 AND 2147483647",
        "expiration_days > 0",
        1,
    );

    assert_ne!(incompatible, original);

    let indexes = database
        .connection
        .prepare("SELECT sql FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'bucket_lifecycle_rules' AND sql IS NOT NULL")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    database
        .connection
        .execute_batch("DROP TABLE bucket_lifecycle_rules")?;

    database.connection.execute_batch(&incompatible)?;

    for index in indexes {
        database.connection.execute_batch(&index)?;
    }

    database.connection.execute_batch(
        "INSERT INTO bucket_lifecycle_rules
         (bucket_id, rule_index, rule_id, is_enabled, filter_kind, filter_prefix, expiration_days)
         SELECT bucket_id, 0, 'preserved', 1, 'prefix', x'ff00', 2147483648 FROM buckets WHERE name = 'photos';
         UPDATE buckets SET lifecycle_generation = zeroblob(16) WHERE name = 'photos';",
    )?;

    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert!(matches!(
        reopened.read_store(),
        Err(MetadataError::Corrupt(
            "store schema differs from the supported format"
        ))
    ));

    let retained: (Option<String>, Vec<u8>, i64) = reopened.connection.query_row(
        "SELECT rule_id, filter_prefix, expiration_days FROM bucket_lifecycle_rules",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    assert_eq!(
        retained,
        (Some("preserved".into()), vec![255, 0], 2_147_483_648)
    );

    assert_eq!(generation(&reopened)?, Some(vec![0; 16]));
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn lifecycle_rows_and_generation_settle_together_after_rollback_lost_reply_and_reopen() -> TestResult
{
    use super::super::super::transaction::Fault;

    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;

    let mut peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let native = Authority::Native;
    let request = request();
    let before = rules("before/");
    let after = rules("after/");
    assert_eq!(database.get_bucket_lifecycle(&native, &request)?, None);
    assert_eq!(generation(&database)?, None);

    for (fault, outcome, committed) in [
        (Fault::BeforeCommit, MutationOutcome::NotCommitted, false),
        (Fault::AfterCommit, MutationOutcome::Unknown, true),
    ] {
        database.put_bucket_lifecycle(&native, &request, &before)?;
        let original_generation = generation(&peer)?.unwrap();
        database.transaction_fault = Some(fault);

        let error = database
            .put_bucket_lifecycle(&native, &request, &after)
            .unwrap_err();

        assert_eq!(error.outcome(), Some(outcome));
        let expected = if committed { &after } else { &before };

        assert_eq!(
            peer.get_bucket_lifecycle(&native, &request)?.as_ref(),
            Some(expected)
        );

        assert_eq!(
            generation(&peer)?.as_ref() != Some(&original_generation),
            committed
        );

        let selected_generation = generation(&peer)?;
        database.transaction_fault = Some(fault);

        let error = database
            .delete_bucket_lifecycle(&native, &request)
            .unwrap_err();

        assert_eq!(error.outcome(), Some(outcome));

        assert_eq!(
            peer.get_bucket_lifecycle(&native, &request)?.as_ref(),
            if committed { None } else { Some(expected) }
        );

        assert_eq!(
            generation(&peer)?,
            if committed { None } else { selected_generation }
        );
    }

    database.put_bucket_lifecycle(&native, &request, &after)?;
    let published_generation = generation(&database)?;
    close(peer)?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(
        reopened.get_bucket_lifecycle(&native, &request)?,
        Some(after)
    );

    assert_eq!(generation(&reopened)?, published_generation);
    reopened.delete_bucket_lifecycle(&native, &request)?;
    reopened.delete_bucket_lifecycle(&native, &request)?;
    assert_eq!(reopened.get_bucket_lifecycle(&native, &request)?, None);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn rejected_later_insert_rolls_back_deleted_rules_and_preserves_generation() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let native = Authority::Native;
    let request = request();
    let before = rules("before/");
    database.put_bucket_lifecycle(&native, &request, &before)?;
    let original_generation = generation(&database)?;

    // A temporary test-only trigger fails after DELETE and the first INSERT.
    database.connection.execute_batch(
        "CREATE TEMP TRIGGER reject_later_rule BEFORE INSERT ON bucket_lifecycle_rules
         WHEN NEW.rule_index = 1 BEGIN SELECT RAISE(ABORT, 'injected later rule failure'); END;",
    )?;

    let error = database
        .put_bucket_lifecycle(&native, &request, &rules("after/"))
        .unwrap_err();

    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

    assert_eq!(
        database.get_bucket_lifecycle(&native, &request)?,
        Some(before)
    );

    assert_eq!(generation(&database)?, original_generation);

    database
        .connection
        .execute_batch("DROP TRIGGER reject_later_rule")?;

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn assigned_ids_and_all_filter_forms_survive_reopen_at_the_rule_limit() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let request = request();
    let mut configured = Vec::new();

    for index in 0..1_000 {
        configured.push(LifecycleRule {
            id: if index % 2 == 0 {
                None
            } else {
                Some(String::new())
            },
            is_enabled: false,
            filter: match index % 3 {
                0 => LifecycleFilter::All,
                1 => LifecycleFilter::Prefix(String::new()),
                _ => LifecycleFilter::LegacyPrefix("é\r/".into()),
            },
            expiration_days: Some(1),
            abort_incomplete_multipart_days: Some(2),
        });
    }

    database.put_bucket_lifecycle(
        &Authority::Native,
        &request,
        &LifecycleConfiguration::new(configured.clone())?,
    )?;

    let selected = database
        .get_bucket_lifecycle(&Authority::Native, &request)?
        .unwrap();

    let mut ids = std::collections::BTreeSet::new();

    for (actual, expected) in selected.rules().iter().zip(&mut configured) {
        let id = actual.id.as_ref().unwrap();
        assert_ne!(id, "");
        assert!(ids.insert(id));
        expected.id.clone_from(&actual.id);
        assert_eq!(actual, expected);
    }

    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(
        reopened.get_bucket_lifecycle(&Authority::Native, &request)?,
        Some(selected)
    );

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn malformed_stored_rules_and_generation_never_become_absent_or_default_rules() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let request = request();

    database
        .connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")?;

    for sql in [
        "UPDATE bucket_lifecycle_rules SET rule_index = 999 WHERE rule_index = 0",
        "UPDATE bucket_lifecycle_rules SET rule_id = '' WHERE rule_index = 0",
        "UPDATE bucket_lifecycle_rules SET rule_id = char(0) WHERE rule_index = 0",
        "UPDATE bucket_lifecycle_rules SET is_enabled = 2 WHERE rule_index = 0",
        "UPDATE bucket_lifecycle_rules SET filter_kind = 'unknown' WHERE rule_index = 0",
        "UPDATE bucket_lifecycle_rules SET filter_kind = 'all' WHERE rule_index = 0",
        "UPDATE bucket_lifecycle_rules SET filter_prefix = x'ff' WHERE rule_index = 0",
        "UPDATE bucket_lifecycle_rules SET expiration_days = 0 WHERE rule_index = 0",
        "UPDATE bucket_lifecycle_rules SET expiration_days = 2147483648 WHERE rule_index = 0",
        "UPDATE bucket_lifecycle_rules SET expiration_days = NULL WHERE rule_index = 0",
        "UPDATE buckets SET lifecycle_generation = NULL",
        "UPDATE buckets SET lifecycle_generation = x'00'",
        "DELETE FROM bucket_lifecycle_rules",
    ] {
        database.put_bucket_lifecycle(&Authority::Native, &request, &rules("present/"))?;
        database.connection.execute(sql, [])?;

        assert_eq!(
            database
                .get_bucket_lifecycle(&Authority::Native, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::StoreCorrupt,
            "{sql}"
        );
    }

    database.delete_bucket_lifecycle(&Authority::Native, &request)?;

    database
        .connection
        .execute_batch("PRAGMA ignore_check_constraints = OFF")?;

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn lifecycle_actions_use_current_permissions_owner_assertions_and_revocation() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut database = initialized(&directory)?;
    let request = request();
    let configuration = rules("private/");
    database.put_bucket_lifecycle(&Authority::Native, &request, &configuration)?;
    let principal = database.create_principal(PrincipalName::new("lifecycle-reader".into())?)?;
    let key = database.create_access_key(principal.principal_id)?;

    let reader = Authority::Http(Arc::new(
        database
            .resolve_application_key(key.access_key.access_key_id.as_str())?
            .verified(),
    ));

    let anonymous = Authority::Http(Arc::new(crate::access::HttpIdentity::anonymous()));

    for (authority_index, authority) in [&reader, &anonymous].into_iter().enumerate() {
        for (operation_index, result) in [
            database.get_bucket_lifecycle(authority, &request).map(drop),
            database.put_bucket_lifecycle(authority, &request, &configuration),
            database.delete_bucket_lifecycle(authority, &request),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                result.unwrap_err().failure(),
                OperationFailure::AccessDenied,
                "authority {authority_index}, operation {operation_index}"
            );
        }
    }

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetLifecycleConfiguration","Resource":"arn:aws:s3:::photos"}}"#,
    )?;

    assert_eq!(
        database.get_bucket_lifecycle(&reader, &request)?,
        Some(configuration.clone())
    );

    for (operation_index, result) in [
        database.put_bucket_lifecycle(&reader, &request, &configuration),
        database.delete_bucket_lifecycle(&reader, &request),
    ]
    .into_iter()
    .enumerate()
    {
        let error = result.unwrap_err();

        assert_eq!(
            error.failure(),
            OperationFailure::AccessDenied,
            "operation {operation_index}"
        );

        assert_eq!(
            error.outcome(),
            Some(MutationOutcome::NotCommitted),
            "operation {operation_index}"
        );
    }

    let owner = database.get_owner_info()?;

    let wrong = BucketAccessRequest {
        expected_owner: Some(
            if owner.account_id() == "000000000000" {
                "111111111111"
            } else {
                "000000000000"
            }
            .into(),
        ),
        ..request.clone()
    };

    for (operation_index, result) in [
        database
            .get_bucket_lifecycle(&Authority::Native, &wrong)
            .map(drop),
        database.put_bucket_lifecycle(&Authority::Native, &wrong, &configuration),
        database.delete_bucket_lifecycle(&Authority::Native, &wrong),
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

    database.put_principal_policy(
        principal.principal_id,
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutLifecycleConfiguration","Resource":"arn:aws:s3:::photos"}}"#,
    )?;

    database.put_bucket_lifecycle(&reader, &request, &configuration)?;

    assert_eq!(
        database
            .get_bucket_lifecycle(&reader, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database.delete_bucket_lifecycle(&reader, &request)?;
    database.delete_access_key(principal.principal_id, &key.access_key.access_key_id)?;

    assert_eq!(
        database
            .put_bucket_lifecycle(&reader, &request, &configuration)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    assert_eq!(
        database.get_bucket_lifecycle(&Authority::Native, &request)?,
        None
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
