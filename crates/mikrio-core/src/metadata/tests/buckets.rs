use super::*;
use crate::object::BucketName;
use crate::{MutationOutcome, OperationFailure};

fn initialized(
    directory: &TestDirectory,
    region: &str,
) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    let mut database = database(directory)?;

    database.initialize(&BootstrapValues::generate(
        StoreId::generate()?,
        region.into(),
        EpochMillis::new(0)?,
    )?)?;

    Ok(database)
}

#[test]
fn repeated_creation_preserves_identity_configuration_and_creation_time()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let mut database = initialized(&directory, "us-east-1")?;
    let bucket = BucketName::parse("attachments")?;
    database.create_bucket(&bucket)?;

    database.connection.execute(
        "UPDATE buckets SET block_public_acls = 0, created_at_ms = 123",
        [],
    )?;

    let first: Vec<u8> =
        database
            .connection
            .query_row("SELECT bucket_id FROM buckets", [], |row| row.get(0))?;

    database.create_bucket(&bucket)?;

    let current: (Vec<u8>, i64, i64) = database.connection.query_row(
        "SELECT bucket_id, block_public_acls, created_at_ms FROM buckets",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    assert_eq!(current, (first.clone(), 0, 123));
    database.delete_bucket(&bucket, None)?;
    database.create_bucket(&bucket)?;

    let replacement: Vec<u8> = database.connection.query_row(
        "SELECT bucket_id FROM buckets WHERE name = 'attachments'",
        [],
        |row| row.get(0),
    )?;

    assert_ne!(first, replacement);

    assert_eq!(
        database.connection.query_row::<i64, _, _>(
            "SELECT count(*) FROM buckets WHERE name IS NULL",
            [],
            |row| row.get(0)
        )?,
        1
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn metadata_commit_faults_preserve_rollback_and_unknown_distinctions()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let mut database = initialized(&directory, "eu-west-1")?;
    let bucket = BucketName::parse("attachments")?;
    database.transaction_fault = Some(super::super::transaction::Fault::BeforeCommit);
    let error = database.create_bucket(&bucket).unwrap_err();
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

    assert_eq!(
        database.head_bucket(&bucket, None).unwrap_err().failure(),
        OperationFailure::NoSuchBucket
    );

    database.transaction_fault = Some(super::super::transaction::Fault::AfterCommit);
    let error = database.create_bucket(&bucket).unwrap_err();
    assert_eq!(error.outcome(), Some(MutationOutcome::Unknown));
    assert_eq!(database.head_bucket(&bucket, None)?, "eu-west-1");

    assert_eq!(
        database.create_bucket(&bucket).unwrap_err().failure(),
        OperationFailure::BucketAlreadyOwnedByYou
    );

    database.transaction_fault = Some(super::super::transaction::Fault::BeforeCommit);

    assert_eq!(
        database.delete_bucket(&bucket, None).unwrap_err().outcome(),
        Some(MutationOutcome::NotCommitted)
    );

    assert_eq!(database.head_bucket(&bucket, None)?, "eu-west-1");
    database.transaction_fault = Some(super::super::transaction::Fault::AfterCommit);

    assert_eq!(
        database.delete_bucket(&bucket, None).unwrap_err().outcome(),
        Some(MutationOutcome::Unknown)
    );

    assert_eq!(
        database.head_bucket(&bucket, None).unwrap_err().failure(),
        OperationFailure::NoSuchBucket
    );

    close(database)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(
        reopened.head_bucket(&bucket, None).unwrap_err().failure(),
        OperationFailure::NoSuchBucket
    );

    reopened.create_bucket(&bucket)?;
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn owner_mismatch_and_corrupt_bucket_do_not_become_missing_success()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let mut database = initialized(&directory, "us-east-1")?;
    let bucket = BucketName::parse("attachments")?;
    database.create_bucket(&bucket)?;

    let owner: String = database.connection.query_row(
        "SELECT owner_account_id FROM store_metadata",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(database.head_bucket(&bucket, Some(&owner))?, "us-east-1");

    let wrong = if owner == "000000000000" {
        "111111111111"
    } else {
        "000000000000"
    };

    assert_eq!(
        database
            .delete_bucket(&bucket, Some(wrong))
            .unwrap_err()
            .failure(),
        OperationFailure::AccessDenied
    );

    database
        .connection
        .execute("UPDATE buckets SET created_at_ms = 253402300800000", [])?;

    assert_eq!(
        database.head_bucket(&bucket, None).unwrap_err().failure(),
        OperationFailure::StoreCorrupt
    );

    close(database)?;
    directory.finish()?;
    Ok(())
}
