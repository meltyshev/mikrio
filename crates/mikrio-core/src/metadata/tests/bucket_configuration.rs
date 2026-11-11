use std::sync::Arc;

use super::*;
use crate::access::{Bootstrap, BucketAccessRequest, PrincipalName};
use crate::object::{Authority, BucketName};
use crate::{MutationOutcome, OperationFailure, PublicAccessBlock};

mod cors;
mod lifecycle;
mod principals;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DENY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:*","Resource":"*"}}"#;
const PUBLIC: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;
const APP_ALLOW: &str =
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#;

fn initialized(directory: &TestDirectory) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    let mut database = database(directory)?;

    database.initialize(&BootstrapValues::generate(
        StoreId::generate()?,
        "us-east-1".into(),
        EpochMillis::new(0)?,
    )?)?;

    database.create_bucket(&BucketName::parse("photos")?)?;
    Ok(database)
}

fn request() -> BucketAccessRequest {
    BucketAccessRequest {
        bucket: BucketName::parse("photos").unwrap(),
        expected_owner: None,
    }
}

fn flags(bits: u8) -> PublicAccessBlock {
    PublicAccessBlock {
        block_public_acls: bits & 1 != 0,
        ignore_public_acls: bits & 2 != 0,
        block_public_policy: bits & 4 != 0,
        restrict_public_buckets: bits & 8 != 0,
    }
}

#[test]
fn independent_flags_and_absence_survive_reopen_without_changing_policy_classification()
-> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = initialized(&directory)?;
    let native = Authority::Native;
    let request = request();

    assert_eq!(
        database.get_public_access_block(&native, &request)?,
        flags(15)
    );

    assert_eq!(
        database
            .get_bucket_policy(&native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchBucketPolicy
    );

    for bits in 0..16 {
        database.put_bucket_policy(&native, &request, DENY)?;
        database.put_public_access_block(&native, &request, flags(bits))?;

        assert_eq!(
            database.get_public_access_block(&native, &request)?,
            flags(bits)
        );

        let install = database.put_bucket_policy(&native, &request, PUBLIC);

        if bits & 4 != 0 {
            let error = install.unwrap_err();
            assert_eq!(error.failure(), OperationFailure::AccessDenied);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

            assert_eq!(
                database.get_bucket_policy(&native, &request)?.document,
                DENY
            );

            assert!(!database.get_bucket_policy_status(&native, &request)?);
        } else {
            install?;
            assert!(database.get_bucket_policy_status(&native, &request)?);
        }
    }

    database.delete_public_access_block(&native, &request)?;
    database.delete_public_access_block(&native, &request)?;

    assert_eq!(
        database
            .get_public_access_block(&native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchPublicAccessBlockConfiguration
    );

    database.put_bucket_policy(&native, &request, PUBLIC)?;
    database.put_public_access_block(&native, &request, flags(15))?;
    assert!(database.get_bucket_policy_status(&native, &request)?);
    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(
        reopened.get_public_access_block(&native, &request)?,
        flags(15)
    );

    assert_eq!(
        reopened.get_bucket_policy(&native, &request)?.document,
        PUBLIC
    );

    assert!(reopened.get_bucket_policy_status(&native, &request)?);
    reopened.delete_bucket_policy(&native, &request)?;
    reopened.delete_bucket_policy(&native, &request)?;

    assert_eq!(
        reopened
            .get_bucket_policy_status(&native, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchBucketPolicy
    );

    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn bootstrap_recovery_is_narrow_and_current_application_authority_can_be_revoked() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = initialized(&directory)?;

    let mut peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let request = request();
    let bootstrap = Bootstrap::generate()?;

    let owner = Authority::Http(Arc::new(
        bootstrap
            .resolve(bootstrap.access_key_id())
            .unwrap()
            .verified(),
    ));

    let principal = database.create_principal(PrincipalName::new("app".into())?)?;
    database.put_principal_policy(principal.principal_id, APP_ALLOW)?;
    let key = database.create_access_key(principal.principal_id)?;

    let application = Authority::Http(Arc::new(
        database
            .resolve_application_key(key.access_key.access_key_id.as_str())?
            .verified(),
    ));

    database.put_bucket_policy(&owner, &request, DENY)?;
    assert_eq!(database.get_bucket_policy(&owner, &request)?.document, DENY);

    for result in [
        database
            .get_bucket_policy_status(&owner, &request)
            .map(drop),
        database.get_public_access_block(&owner, &request).map(drop),
        database.delete_public_access_block(&owner, &request),
        database.get_bucket_ownership_controls(&owner, &request),
        database.put_bucket_ownership_controls(&owner, &request),
        database.get_bucket_acl(&owner, &request).map(drop),
        database.get_bucket_policy(&application, &request).map(drop),
        database.delete_bucket_policy(&application, &request),
    ] {
        assert_eq!(
            result.unwrap_err().failure(),
            OperationFailure::AccessDenied
        );
    }

    assert_eq!(
        database
            .put_bucket_policy(&owner, &request, PUBLIC)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    let wrong_owner = if database.get_owner_info()?.account_id() == "000000000000" {
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
            .delete_bucket_policy(&owner, &wrong)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database.delete_bucket_policy(&owner, &request)?;
    database.get_public_access_block(&application, &request)?;
    database.put_bucket_ownership_controls(&application, &request)?;
    database.get_bucket_ownership_controls(&application, &request)?;

    assert_eq!(
        database.get_bucket_acl(&application, &request)?,
        database.get_owner_info()?
    );

    peer.delete_access_key(principal.principal_id, &key.access_key.access_key_id)?;

    assert_eq!(
        database
            .get_public_access_block(&application, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    bootstrap.close();

    assert_eq!(
        database
            .delete_bucket_policy(&owner, &request)
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    close(peer)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn whole_configuration_rollback_and_lost_commit_acknowledgment_remain_distinct() -> TestResult {
    use super::super::transaction::Fault;

    let directory = TestDirectory::new()?;
    let mut database = initialized(&directory)?;

    let mut peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let request = request();
    let native = Authority::Native;
    database.delete_public_access_block(&native, &request)?;
    database.put_bucket_policy(&native, &request, DENY)?;

    for (fault, outcome, committed) in [
        (Fault::BeforeCommit, MutationOutcome::NotCommitted, false),
        (Fault::AfterCommit, MutationOutcome::Unknown, true),
    ] {
        database.transaction_fault = Some(fault);

        assert_eq!(
            database
                .put_bucket_policy(&native, &request, PUBLIC)
                .unwrap_err()
                .outcome(),
            Some(outcome)
        );

        assert_eq!(
            peer.get_bucket_policy(&native, &request)?.document,
            if committed { PUBLIC } else { DENY }
        );

        database.transaction_fault = Some(fault);

        assert_eq!(
            database
                .put_public_access_block(&native, &request, flags(10))
                .unwrap_err()
                .outcome(),
            Some(outcome)
        );

        if committed {
            assert_eq!(peer.get_public_access_block(&native, &request)?, flags(10));
        } else {
            assert_eq!(
                peer.get_public_access_block(&native, &request)
                    .unwrap_err()
                    .failure(),
                OperationFailure::NoSuchPublicAccessBlockConfiguration
            );
        }
    }

    let invalid = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"*"},{"Effect":"Allow","Principal":"*","Action":"s3:Unknown","Resource":"*"}]}"#;

    let rejected = database
        .put_bucket_policy(&native, &request, invalid)
        .unwrap_err();

    assert_eq!(rejected.failure(), OperationFailure::InvalidArgument);
    assert_eq!(rejected.outcome(), Some(MutationOutcome::NotCommitted));
    assert_eq!(peer.get_bucket_policy(&native, &request)?.document, PUBLIC);

    for (fault, outcome, committed) in [
        (Fault::BeforeCommit, MutationOutcome::NotCommitted, false),
        (Fault::AfterCommit, MutationOutcome::Unknown, true),
    ] {
        database.transaction_fault = Some(fault);

        assert_eq!(
            database
                .delete_bucket_policy(&native, &request)
                .unwrap_err()
                .outcome(),
            Some(outcome)
        );

        if committed {
            assert_eq!(
                peer.get_bucket_policy(&native, &request)
                    .unwrap_err()
                    .failure(),
                OperationFailure::NoSuchBucketPolicy
            );
        } else {
            assert_eq!(peer.get_bucket_policy(&native, &request)?.document, PUBLIC);
        }

        database.transaction_fault = Some(fault);

        assert_eq!(
            database
                .delete_public_access_block(&native, &request)
                .unwrap_err()
                .outcome(),
            Some(outcome)
        );

        if committed {
            assert_eq!(
                peer.get_public_access_block(&native, &request)
                    .unwrap_err()
                    .failure(),
                OperationFailure::NoSuchPublicAccessBlockConfiguration
            );
        } else {
            assert_eq!(peer.get_public_access_block(&native, &request)?, flags(10));
        }
    }

    close(peer)?;
    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn malformed_stored_policy_or_partial_flags_never_become_recovery_authority() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut database = initialized(&directory)?;
    let request = request();
    let bootstrap = Bootstrap::generate()?;

    let owner = Authority::Http(Arc::new(
        bootstrap
            .resolve(bootstrap.access_key_id())
            .unwrap()
            .verified(),
    ));

    database
        .connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")?;

    for sql in [
        "UPDATE buckets SET policy_generation = zeroblob(16), policy_json = '{}'",
        "UPDATE buckets SET policy_generation = NULL, policy_json = NULL, block_public_acls = NULL",
    ] {
        database.connection.execute_batch(sql)?;

        assert_eq!(
            database
                .get_bucket_policy(&owner, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::StoreCorrupt
        );

        let rejected = database.delete_bucket_policy(&owner, &request).unwrap_err();
        assert_eq!(rejected.failure(), OperationFailure::StoreCorrupt);
        assert_eq!(rejected.outcome(), Some(MutationOutcome::NotCommitted));

        assert_eq!(
            database
                .get_public_access_block(&Authority::Native, &request)
                .unwrap_err()
                .failure(),
            OperationFailure::StoreCorrupt
        );
    }

    close(database)?;
    directory.finish()?;
    Ok(())
}
