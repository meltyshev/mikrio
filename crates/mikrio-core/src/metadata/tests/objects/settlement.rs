//! Actual SQLite transaction refusals, not a disk-error or power-loss simulation.

use std::sync::{Arc, Mutex};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};

use super::*;
use crate::object::Authority;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Copy)]
enum Refusal {
    Rollback,
    CommitAndRollback,
}

fn refuse_settlement(
    database: &mut MetadataDatabase,
    refusal: Refusal,
) -> rusqlite::Result<Arc<Mutex<Vec<TransactionOperation>>>> {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&observed);

    database
        .connection
        .authorizer(Some(move |context: AuthContext<'_>| {
            let AuthAction::Transaction { operation } = context.action else {
                return Authorization::Allow;
            };

            recorded.lock().unwrap().push(operation);

            if operation == TransactionOperation::Rollback
                || (matches!(refusal, Refusal::CommitAndRollback)
                    && operation != TransactionOperation::Begin)
            {
                return Authorization::Deny;
            }

            Authorization::Allow
        }))?;

    Ok(observed)
}

fn predecessor(
    directory: &TestDirectory,
) -> Result<(MetadataDatabase, PutRequest), Box<dyn std::error::Error>> {
    let (mut database, request) = setup(directory)?;
    let blob = BlobId::from_bytes([21; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, &request, blob)?
        .unwrap();

    database.publish_put(
        &Authority::Native,
        &request,
        bucket,
        blob,
        &facts(&request, b"original")?,
    )?;

    Ok((database, request))
}

fn refuses_later_work(database: &mut MetadataDatabase) {
    assert!(database.unusable);
    assert!(!database.connection.is_autocommit());
    let mut entered = false;

    let error = database
        .write(|_| {
            entered = true;
            Ok(())
        })
        .unwrap_err();

    assert!(!entered);
    assert_eq!(error.failure(), OperationFailure::IoError);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

    let error = database
        .read(|_| {
            entered = true;
            Ok(())
        })
        .unwrap_err();

    assert!(!entered);
    assert_eq!(error.failure(), OperationFailure::IoError);
    assert_eq!(error.outcome(), None);
}

fn reopen_twice(directory: &TestDirectory, request: &PutRequest) -> TestResult {
    for _ in 0..2 {
        let mut reopened = MetadataDatabase::open(
            &directory.path().join("metadata.sqlite3"),
            MetadataOpen::Existing,
        )?;

        let current = reopened.select_object(&Authority::Native, &read(request))?;
        assert_eq!(current.blob_id, BlobId::from_bytes([21; 16]));
        assert_eq!(current.tags, crate::object::ObjectTags::default());
        reopened.create_bucket(&BucketName::parse("still-writable")?)?;
        close(reopened)?;
    }

    Ok(())
}

#[test]
fn failed_commit_and_rollback_cannot_promote_provisional_ready_witness() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = predecessor(&directory)?;

    let mut peer = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let output = BlobId::from_bytes([22; 16]);

    let bucket = database
        .reserve_put(&Authority::Native, &request, output)?
        .unwrap();

    let observed = refuse_settlement(&mut database, Refusal::CommitAndRollback)?;

    let error = database
        .publish_put(
            &Authority::Native,
            &request,
            bucket,
            output,
            &facts(&request, b"replacement")?,
        )
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::IoError);
    assert_eq!(error.outcome(), Some(MutationOutcome::Unknown));

    // The authorizer maps SQLite's COMMIT action to Unknown. Both real ending
    // statements were attempted, and SQLite refused them during preparation.
    assert_eq!(
        *observed.lock().unwrap(),
        [
            TransactionOperation::Begin,
            TransactionOperation::Unknown,
            TransactionOperation::Rollback,
        ]
    );

    refuses_later_work(&mut database);

    let provisional: String = database.connection.query_row(
        "SELECT state FROM blobs WHERE blob_id = ?1",
        [output.as_bytes().as_slice()],
        |row| row.get(0),
    )?;

    assert_eq!(provisional, "ready");

    assert_eq!(
        peer.select_object(&Authority::Native, &read(&request))?
            .blob_id,
        BlobId::from_bytes([21; 16])
    );

    let committed: String = peer.connection.query_row(
        "SELECT state FROM blobs WHERE blob_id = ?1",
        [output.as_bytes().as_slice()],
        |row| row.get(0),
    )?;

    assert_eq!(committed, "reserved");

    close(database)?;
    close(peer)?;
    reopen_twice(&directory, &request)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn failed_rollback_after_work_error_disables_connection_before_later_commands() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = predecessor(&directory)?;
    let observed = refuse_settlement(&mut database, Refusal::Rollback)?;

    let error = database
        .write(|transaction| {
            transaction.execute("UPDATE objects SET tags_json = '{\"pending\":\"yes\"}'", [])?;
            // A real NOT NULL constraint failure after an actual provisional change.
            transaction.execute("INSERT INTO objects (bucket_id) VALUES (NULL)", [])?;
            Ok(())
        })
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::IoError);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

    assert_eq!(
        *observed.lock().unwrap(),
        [TransactionOperation::Begin, TransactionOperation::Rollback]
    );

    refuses_later_work(&mut database);

    let provisional: String =
        database
            .connection
            .query_row("SELECT tags_json FROM objects", [], |row| row.get(0))?;

    assert_eq!(provisional, "{\"pending\":\"yes\"}");

    close(database)?;
    reopen_twice(&directory, &request)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn failed_read_rollback_discards_snapshot_and_disables_connection() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = predecessor(&directory)?;
    let observed = refuse_settlement(&mut database, Refusal::Rollback)?;

    let error = database
        .select_object(&Authority::Native, &read(&request))
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::IoError);
    assert_eq!(error.outcome(), None);

    assert_eq!(
        *observed.lock().unwrap(),
        [TransactionOperation::Begin, TransactionOperation::Rollback]
    );

    refuses_later_work(&mut database);

    close(database)?;
    reopen_twice(&directory, &request)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn explicit_sqlite_settlement_cannot_reenable_a_discarded_metadata_connection() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let (mut database, request) = predecessor(&directory)?;
    let identity = database.read_store()?;
    let observed = refuse_settlement(&mut database, Refusal::Rollback)?;

    let error = database
        .select_object(&Authority::Native, &read(&request))
        .unwrap_err();

    assert_eq!(error.failure(), OperationFailure::IoError);
    assert_eq!(error.outcome(), None);

    assert_eq!(
        *observed.lock().unwrap(),
        [TransactionOperation::Begin, TransactionOperation::Rollback]
    );

    refuses_later_work(&mut database);

    // Settle the actual SQLite transaction directly, without changing the
    // wrapper's discard decision. This private fixture is not a recovery API.
    database
        .connection
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;

    database.connection.execute_batch("ROLLBACK")?;
    assert!(database.connection.is_autocommit());
    assert!(database.unusable);

    assert!(matches!(
        database.read_store(),
        Err(MetadataError::Corrupt(_))
    ));

    let attempted = BucketName::parse("discarded-connection-write")?;
    let error = database.create_bucket(&attempted).unwrap_err();
    assert_eq!(error.failure(), OperationFailure::IoError);
    assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
    close(database)?;

    let reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(reopened.read_store()?, identity);

    let unexpected: i64 = reopened.connection.query_row(
        "SELECT count(*) FROM buckets WHERE name = 'discarded-connection-write'",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(unexpected, 0);
    close(reopened)?;
    reopen_twice(&directory, &request)?;
    directory.finish()?;
    Ok(())
}
