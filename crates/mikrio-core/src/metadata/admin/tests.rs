use super::*;
use crate::access::CredentialClass;
use crate::metadata::transaction::Fault;
use crate::test_directory::TestDirectory;
use crate::{BootstrapValues, MetadataOpen, MutationOutcome};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const ALLOW: &str = r#"{ "Version":"2012-10-17", "Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;

mod faults;
mod pages;

fn database(directory: &TestDirectory) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    let path = directory.path().join("metadata.sqlite3");
    std::fs::File::create_new(&path)?;
    let mut database = MetadataDatabase::open(&path, MetadataOpen::Initialize)?;

    database.initialize(&BootstrapValues::generate(
        StoreId::generate()?,
        "us-east-1".into(),
        EpochMillis::new(0)?,
    )?)?;

    Ok(database)
}

fn close(database: MetadataDatabase) -> TestResult {
    database.close().map_err(|(_, error)| error.into())
}

#[test]
fn identities_require_explicit_revocation_and_reused_names_never_rebind() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = database(&directory)?;
    let owner = database.get_owner_info()?;
    let first = database.create_principal(PrincipalName::new("Application".into())?)?;
    assert!(first.is_enabled);
    let duplicate = database.create_principal(first.name.clone()).unwrap_err();

    assert_eq!(
        duplicate.failure(),
        OperationFailure::PrincipalAlreadyExists
    );

    assert_eq!(duplicate.outcome(), Some(MutationOutcome::NotCommitted));
    let other = database.create_principal(PrincipalName::new("application".into())?)?;
    let key = database.create_access_key(first.principal_id)?;

    assert_eq!(
        CredentialClass::parse(key.access_key.access_key_id.as_str())?,
        CredentialClass::Application
    );

    assert!(!format!("{key:?}").contains(key.secret_access_key.expose()));

    assert_eq!(
        database
            .resolve_application_key(key.access_key.access_key_id.as_str())?
            .secret()
            .expose(),
        key.secret_access_key.expose()
    );

    let blocked = database.delete_principal(first.principal_id).unwrap_err();
    assert_eq!(blocked.failure(), OperationFailure::PrincipalHasAccessKeys);
    assert_eq!(blocked.outcome(), Some(MutationOutcome::NotCommitted));

    assert_eq!(
        database
            .delete_access_key(other.principal_id, &key.access_key.access_key_id)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    database.resolve_application_key(key.access_key.access_key_id.as_str())?;
    database.delete_access_key(first.principal_id, &key.access_key.access_key_id)?;
    database.delete_access_key(first.principal_id, &key.access_key.access_key_id)?;

    assert!(matches!(
        database.resolve_application_key(key.access_key.access_key_id.as_str()),
        Err(crate::AccessError::Denied)
    ));

    database.delete_principal(first.principal_id)?;

    assert_eq!(
        database
            .get_principal(first.principal_id)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchPrincipal
    );

    let replacement = database.create_principal(first.name)?;
    assert_ne!(replacement.principal_id, first.principal_id);
    assert_eq!(database.get_owner_info()?, owner);
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn disabled_principals_can_prepare_keys_but_only_current_keys_reenable() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = database(&directory)?;
    let principal = database.create_principal(PrincipalName::new("app".into())?)?;
    let disabled = database.set_principal_enabled(principal.principal_id, false)?;
    assert!(!disabled.is_enabled);
    assert_eq!(disabled.created_at, principal.created_at);
    let first = database.create_access_key(principal.principal_id)?;
    let second = database.create_access_key(principal.principal_id)?;

    assert_ne!(
        first.access_key.access_key_id,
        second.access_key.access_key_id
    );

    assert_ne!(
        first.secret_access_key.expose(),
        second.secret_access_key.expose()
    );

    for key in [&first, &second] {
        assert!(matches!(
            database.resolve_application_key(key.access_key.access_key_id.as_str()),
            Err(crate::AccessError::Denied)
        ));
    }

    database.delete_access_key(principal.principal_id, &first.access_key.access_key_id)?;
    database.set_principal_enabled(principal.principal_id, true)?;

    assert!(matches!(
        database.resolve_application_key(first.access_key.access_key_id.as_str()),
        Err(crate::AccessError::Denied)
    ));

    database.resolve_application_key(second.access_key.access_key_id.as_str())?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn policies_preserve_exact_text_and_invalid_replacement_is_atomic() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = database(&directory)?;
    let principal = database.create_principal(PrincipalName::new("app".into())?)?;
    let id = principal.principal_id;

    assert!(matches!(
        database.get_principal_policy(id),
        Err(error) if error.failure() == OperationFailure::NoSuchPrincipalPolicy
    ));

    database.delete_principal_policy(id)?;
    database.put_principal_policy(id, ALLOW)?;
    assert_eq!(database.get_principal_policy(id)?.document, ALLOW);
    let invalid = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"},{"Effect":"Allow","Action":"s3:Unknown","Resource":"*"}]}"#;

    assert_eq!(
        database
            .put_principal_policy(id, invalid)
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    assert_eq!(database.get_principal_policy(id)?.document, ALLOW);
    database.delete_principal_policy(id)?;
    database.delete_principal_policy(id)?;
    database.delete_principal(id)?;

    assert!(matches!(
        database.get_principal_policy(id),
        Err(error) if error.failure() == OperationFailure::NoSuchPrincipal
    ));

    assert_eq!(
        database.delete_principal_policy(id).unwrap_err().failure(),
        OperationFailure::NoSuchPrincipal
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn metadata_faults_preserve_whole_states_without_recovering_lost_secrets() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = database(&directory)?;
    let principal = database.create_principal(PrincipalName::new("app".into())?)?;
    let id = principal.principal_id;
    database.put_principal_policy(id, ALLOW)?;
    database.transaction_fault = Some(Fault::BeforeCommit);
    let failure = database.delete_principal_policy(id).unwrap_err();
    assert_eq!(failure.outcome(), Some(MutationOutcome::NotCommitted));
    assert_eq!(database.get_principal_policy(id)?.document, ALLOW);
    database.transaction_fault = Some(Fault::AfterCommit);
    let failure = database.delete_principal_policy(id).unwrap_err();
    assert_eq!(failure.outcome(), Some(MutationOutcome::Unknown));

    assert!(matches!(
        database.get_principal_policy(id),
        Err(error) if error.failure() == OperationFailure::NoSuchPrincipalPolicy
    ));

    database.transaction_fault = Some(Fault::BeforeCommit);

    assert_eq!(
        database.create_access_key(id).unwrap_err().outcome(),
        Some(MutationOutcome::NotCommitted)
    );

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM access_keys", [], |row| {
                row.get::<_, i64>(0)
            })?,
        0
    );

    database.transaction_fault = Some(Fault::AfterCommit);
    let lost = database.create_access_key(id).unwrap_err();
    assert_eq!(lost.outcome(), Some(MutationOutcome::Unknown));

    assert_eq!(
        database
            .connection
            .query_row("SELECT count(*) FROM access_keys", [], |row| {
                row.get::<_, i64>(0)
            })?,
        1
    );

    assert_eq!(
        database.delete_principal(id).unwrap_err().failure(),
        OperationFailure::PrincipalHasAccessKeys
    );

    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(reopened.get_principal(id)?, principal);

    assert_eq!(
        reopened.delete_principal(id).unwrap_err().failure(),
        OperationFailure::PrincipalHasAccessKeys
    );

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn live_pages_are_bounded_scoped_and_survive_restart() -> TestResult {
    use crate::access::AdminPageSize;
    let directory = TestDirectory::new()?;
    let mut database = database(&directory)?;
    let mut expected = Vec::new();

    for name in ["one", "two", "three"] {
        expected.push(database.create_principal(PrincipalName::new(name.into())?)?);
    }

    expected.sort_by_key(|principal| *principal.principal_id.as_bytes());
    let first = database.list_principals(AdminPageSize::new(1)?, None)?;
    assert_eq!(first.principals, expected[..1]);
    let token = first.next_continuation_token.unwrap();

    let keys = (0..3)
        .map(|_| database.create_access_key(expected[0].principal_id))
        .collect::<Result<Vec<_>, _>>()?;

    let key_page =
        database.list_access_keys(expected[0].principal_id, AdminPageSize::new(2)?, None)?;

    assert_eq!(key_page.access_keys.len(), 2);
    let key_token = key_page.next_continuation_token.unwrap();

    assert_eq!(
        database
            .list_access_keys(
                expected[1].principal_id,
                AdminPageSize::new(2)?,
                Some(&key_token)
            )
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    assert_eq!(
        database
            .list_principals(AdminPageSize::new(1)?, Some(&key_token))
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    assert_eq!(
        database
            .list_access_keys(
                expected[0].principal_id,
                AdminPageSize::new(2)?,
                Some(&token)
            )
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    for invalid in [
        String::new(),
        "a".repeat(261),
        token.get(..token.len() - 1).unwrap().into(),
        format!("x{}", token.get(1..).unwrap()),
    ] {
        assert_eq!(
            database
                .list_principals(AdminPageSize::default(), Some(&invalid))
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidArgument
        );
    }

    // A deletion before the cursor does not shift subsequent pages.
    for key in keys {
        database.delete_access_key(expected[0].principal_id, &key.access_key.access_key_id)?;
    }

    database.delete_principal(expected[0].principal_id)?;
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let remaining = reopened.list_principals(AdminPageSize::new(2)?, Some(&token))?;
    assert_eq!(remaining.principals, expected[1..]);
    assert_eq!(remaining.next_continuation_token, None);

    let empty_keys =
        reopened.list_access_keys(expected[1].principal_id, AdminPageSize::new(1)?, None)?;

    assert_eq!(empty_keys.access_keys, []);
    assert_eq!(empty_keys.next_continuation_token, None);
    close(reopened)?;

    let other_directory = TestDirectory::new()?;
    let mut other = self::database(&other_directory)?;

    assert_eq!(
        other
            .list_principals(AdminPageSize::default(), Some(&token))
            .unwrap_err()
            .failure(),
        OperationFailure::InvalidArgument
    );

    close(other)?;
    directory.finish()?;
    other_directory.finish()?;
    Ok(())
}

#[test]
fn page_limits_and_key_inventory_preserve_binary_order_without_secrets() -> TestResult {
    use crate::access::AdminPageSize;
    let directory = TestDirectory::new()?;
    let mut database = database(&directory)?;
    let principal = database.create_principal(PrincipalName::new("app".into())?)?;
    let mut expected = Vec::new();
    let mut secrets = Vec::new();

    for _ in 0..101 {
        let key = database.create_access_key(principal.principal_id)?;
        secrets.push(key.secret_access_key.expose().to_owned());
        expected.push(key.access_key);
    }

    expected.sort_by(|left, right| {
        left.access_key_id
            .as_str()
            .cmp(right.access_key_id.as_str())
    });

    for size in [1, 100, 1000] {
        let mut actual = Vec::new();
        let mut token = None;

        loop {
            let page = database.list_access_keys(
                principal.principal_id,
                AdminPageSize::new(size)?,
                token.as_deref(),
            )?;

            assert!(page.access_keys.len() <= usize::try_from(size)?);
            let diagnostic = format!("{page:?}");
            assert!(secrets.iter().all(|secret| !diagnostic.contains(secret)));
            actual.extend(page.access_keys);
            token = page.next_continuation_token;

            if token.is_none() {
                break;
            }
        }

        assert_eq!(actual, expected);
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn malformed_persisted_administration_values_are_not_empty_inventory() -> TestResult {
    use crate::access::AdminPageSize;
    let directory = TestDirectory::new()?;
    let mut database = database(&directory)?;
    let principal = database.create_principal(PrincipalName::new("app".into())?)?;

    database
        .connection
        .execute("UPDATE principals SET name = ?1", ["bad\nname"])?;

    assert_eq!(
        database
            .get_principal(principal.principal_id)
            .unwrap_err()
            .failure(),
        OperationFailure::StoreCorrupt
    );

    assert_eq!(
        database
            .list_principals(AdminPageSize::default(), None)
            .unwrap_err()
            .failure(),
        OperationFailure::StoreCorrupt
    );

    database.connection.execute(
        "UPDATE principals SET name = 'app', policy_generation = ?1, policy_json = ?2",
        params![[1_u8; 16].as_slice(), r#"{"invalid":true}"#],
    )?;

    assert!(matches!(
        database.get_principal_policy(principal.principal_id),
        Err(error) if error.failure() == OperationFailure::StoreCorrupt
    ));

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn concurrent_key_creation_and_principal_deletion_never_leave_orphans() -> TestResult {
    use std::sync::{Arc, Barrier};
    let directory = TestDirectory::new()?;
    let mut database = database(&directory)?;

    for index in 0..16 {
        let principal = database.create_principal(PrincipalName::new(format!("app-{index}"))?)?;
        let path = directory.path().join("metadata.sqlite3");
        let barrier = Arc::new(Barrier::new(2));

        let (creation, deletion) = std::thread::scope(|scope| {
            let first_path = path.clone();
            let first_barrier = Arc::clone(&barrier);

            let creator = scope.spawn(move || {
                let mut peer = MetadataDatabase::open(&first_path, MetadataOpen::Existing).unwrap();
                first_barrier.wait();
                let result = peer.create_access_key(principal.principal_id);
                peer.close().map_err(|(_, error)| error).unwrap();
                result
            });

            let remover = scope.spawn(move || {
                let mut peer = MetadataDatabase::open(&path, MetadataOpen::Existing).unwrap();
                barrier.wait();
                let result = peer.delete_principal(principal.principal_id);
                peer.close().map_err(|(_, error)| error).unwrap();
                result
            });

            (creator.join().unwrap(), remover.join().unwrap())
        });

        match (creation, deletion) {
            (Ok(key), Err(error)) => {
                assert_eq!(error.failure(), OperationFailure::PrincipalHasAccessKeys);
                assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
                database.resolve_application_key(key.access_key.access_key_id.as_str())?;

                database
                    .delete_access_key(principal.principal_id, &key.access_key.access_key_id)?;

                database.delete_principal(principal.principal_id)?;
            }
            (Err(error), Ok(())) => {
                assert_eq!(error.failure(), OperationFailure::NoSuchPrincipal);
                assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
            }
            result => {
                return Err(std::io::Error::other(format!(
                    "nonserializable administration result: {result:?}"
                ))
                .into());
            }
        }

        assert!(
            !database
                .connection
                .prepare("PRAGMA foreign_key_check")?
                .exists([])?
        );
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}
