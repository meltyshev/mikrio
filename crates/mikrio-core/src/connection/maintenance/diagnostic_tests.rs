//! Missing identity and corrupt retirement never authorize invented reports or deletion.

use super::*;
use crate::GuardManager;
use crate::connection::Connection;
use crate::integrity::{PutIntegrity, PutIntegrityInput};
use crate::object::{
    BucketName, ObjectKey, PutRequest, ReadConditions, ReadConditionsInput, ReadRequest,
    Representation, UserMetadata, WriteConditions,
};
use crate::test_directory::TestDirectory;
use std::io;
use std::sync::atomic::Ordering;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

fn put() -> PutRequest {
    PutRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse("selected").unwrap(),
        expected_owner: None,
        content_length: None,
        representation: Representation::default(),
        metadata: UserMetadata::default(),
        tags: None,
        integrity: PutIntegrity::new(PutIntegrityInput::default()).unwrap(),
        conditions: WriteConditions::new(None, None).unwrap(),
    }
}

fn read() -> ReadRequest {
    ReadRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse("selected").unwrap(),
        expected_owner: None,
        conditions: ReadConditions::new(ReadConditionsInput::default()).unwrap(),
        range: None,
        part_number: None,
        response: Representation::default(),
    }
}

fn pause(connection: &Connection, value: bool) {
    connection
        .owner
        .state
        .context
        .test_hooks
        .pause_maintenance
        .store(value, Ordering::SeqCst);
}

#[test]
fn context_without_store_identity_emits_no_fabricated_diagnostic_and_preserves_bytes() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        pause(&connection, true);

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put(), b"acknowledged live bytes".as_slice())
            .await?;

        let facts = connection.head_object(read()).await?;
        connection.close().await?;
        drop(connection);
        let database = std::fs::read(path.join("metadata.sqlite3"))?;
        let marker = std::fs::read(path.join("store.json"))?;

        let payload = path
            .join("objects")
            .join(format!("{:02x}", facts.blob_id.shard()))
            .join(facts.blob_id.to_string());

        let bytes = std::fs::read(&payload)?;
        assert_eq!(bytes, b"acknowledged live bytes");

        // This is the real context state before Open has assigned an identity.
        // The owned worker has no store and receives no fabricated Open reply.
        let context = Context::start(path.clone(), GuardManager::open(&path)?.identity())?;
        assert!(context.store_id.get().is_none());
        let receiver = context.diagnostics.receiver();
        let before = context.diagnostics.stats();
        let missing = std::fs::File::open(path.join("absent-diagnostic-input")).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        let error = OperationError::caused(OperationFailure::IoError, missing);
        record(&context, &error);
        assert_eq!(context.diagnostics.stats(), before);
        assert!(receiver.take().is_none());

        let worker = context
            .worker
            .lock()
            .map_err(|_| "poisoned worker handle")?
            .take()
            .ok_or("missing worker handle")?;

        let mut terminal = context.terminal.clone();
        drop(context);

        tokio::time::timeout(Duration::from_secs(2), async {
            while terminal.borrow().is_none() {
                terminal.changed().await?;
            }

            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await??;

        assert!(matches!(terminal.borrow().as_ref(), Some(Ok(()))));
        worker.join().map_err(|_| "pre-open worker panicked")?;
        assert!(receiver.is_closed());
        assert_eq!(std::fs::read(path.join("metadata.sqlite3"))?, database);
        assert_eq!(std::fs::read(path.join("store.json"))?, marker);
        assert_eq!(std::fs::read(&payload)?, bytes);

        // The same error is observable once an ordinary open assigns the real ID.
        let reopened = Connection::open(path, None).await?;
        pause(&reopened, true);
        let context = &reopened.owner.state.context;
        let receiver = context.diagnostics.receiver();
        record(context, &error);

        let (diagnostic, _) = receiver
            .take()
            .ok_or("known identity lost its diagnostic")?;

        assert_eq!(Some(&diagnostic.store_id()), context.store_id.get());
        assert_eq!(diagnostic.work(), Work::Cleanup);
        assert_eq!(diagnostic.failure(), Some("io"));
        assert_eq!(diagnostic.occurrences(), 1);
        assert!(receiver.take().is_none());
        let (_, mut body) = reopened.get_object(read()).await?;
        assert_eq!(body.read_all(64).await?, bytes);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn scheduler_reports_corrupt_retirement_without_erasing_a_live_object() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        pause(&connection, true);

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put(), b"acknowledged live bytes".as_slice())
            .await?;

        let facts = connection.head_object(read()).await?;
        let receiver = connection.owner.state.context.diagnostics.receiver();
        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        let bucket_id: Vec<u8> = sql.query_row(
            "SELECT bucket_id FROM buckets WHERE name = 'attachments'",
            [],
            |row| row.get(0),
        )?;

        // Deliberately corrupt only an owned fixture: a retired bucket still has
        // a current object. The contract requires refusal, never deletion authority.
        assert_eq!(
            sql.execute(
                "UPDATE buckets SET name = NULL WHERE bucket_id = ?1",
                [bucket_id.as_slice()],
            )?,
            1
        );

        pause(&connection, false);
        connection.owner.state.context.wake_maintenance.notify_one();

        let diagnostic = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((diagnostic, _)) = receiver.take()
                    && diagnostic.failure() == Some("corruption")
                {
                    return diagnostic;
                }

                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await?;

        pause(&connection, true);
        assert_eq!(diagnostic.work(), Work::Cleanup);

        assert_eq!(
            Some(&diagnostic.store_id()),
            connection.owner.state.context.store_id.get()
        );

        assert!(diagnostic.occurrences() >= 1);

        let row: (String, i64, i64) = sql.query_row(
            "SELECT state, gc_pending, (SELECT count(*) FROM objects WHERE blob_id = ?1) FROM blobs WHERE blob_id = ?1",
            [facts.blob_id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        assert_eq!(row, ("ready".to_owned(), 0, 1));

        let payload_path = path
            .join("objects")
            .join(format!("{:02x}", facts.blob_id.shard()))
            .join(facts.blob_id.to_string());

        assert_eq!(std::fs::read(payload_path)?, b"acknowledged live bytes");

        assert_eq!(
            sql.execute(
                "UPDATE buckets SET name = 'attachments' WHERE bucket_id = ?1 AND name IS NULL",
                [bucket_id.as_slice()],
            )?,
            1
        );

        let check: String = sql.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        assert_eq!(check, "ok");
        sql.close().map_err(|(_, error)| error)?;
        let (_, mut body) = connection.get_object(read()).await?;
        assert_eq!(body.read_all(64).await?, b"acknowledged live bytes");
        connection.close().await?;
        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read()).await?;
        assert_eq!(body.read_all(64).await?, b"acknowledged live bytes");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
