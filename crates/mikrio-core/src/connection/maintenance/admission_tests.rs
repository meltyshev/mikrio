//! Real queue pressure and SQLite authorization refusal preserve cleanup authority.

use super::*;
use crate::connection::Connection;
use crate::integrity::{PutIntegrity, PutIntegrityInput};
use crate::object::{
    BucketName, ObjectKey, PutRequest, ReadConditions, ReadConditionsInput, ReadRequest,
    Representation, UserMetadata, WriteConditions,
};
use crate::test_directory::TestDirectory;
use crate::test_process as process;
use std::io::{self, BufRead as _, Write as _};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::mpsc::{OwnedPermit, Sender};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const CHILD: &str = "connection::maintenance::admission_tests::queue_pressure_child";

struct Pressure {
    sender: Sender<WorkerCommand>,
    permits: Vec<OwnedPermit<WorkerCommand>>,
    armed: bool,
    hit: bool,
}

static PRESSURE: Mutex<Option<Pressure>> = Mutex::new(None);

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

fn saturate(point: &str) -> io::Result<()> {
    if point != "gc.selected" {
        return Ok(());
    }

    let mut state = PRESSURE
        .lock()
        .map_err(|_| io::Error::other("poisoned pressure fixture"))?;

    let Some(state) = state.as_mut() else {
        return Ok(());
    };

    if !state.armed {
        return Ok(());
    }

    assert!(!state.hit);

    while let Ok(permit) = state.sender.clone().try_reserve_owned() {
        state.permits.push(permit);
    }

    assert_eq!(state.permits.len(), crate::connection::QUEUE_CAPACITY);
    assert_eq!(state.sender.capacity(), 0);
    state.hit = true;
    Ok(())
}

#[test]
fn saturated_retry_admission_preserves_pending_blob_and_reclaims_after_capacity_returns()
-> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    // Prepare only the empty durable layout before the unchanged child deadline.
    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let mut child = process::Probe::start(directory.path(), "pressure", CHILD)?;
    assert_eq!(child.observation()?, "VERIFIED");
    child.finish()?;
    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "owned subprocess entry; invoked by the pressure parent"]
fn queue_pressure_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?)
            .join("store");

    assert_eq!(std::env::var("mikrio_probe_mode")?, "pressure");
    crate::protocol_fault::install(saturate)?;

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        pause(&connection, true);

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put(), b"predecessor bytes".as_slice())
            .await?;

        let old = connection.head_object(read()).await?;

        connection
            .put_object(put(), b"acknowledged successor".as_slice())
            .await?;

        let current = connection.head_object(read()).await?;
        assert_ne!(old.blob_id, current.blob_id);
        let sql = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

        let pending = || {
            sql.query_row(
                "SELECT state, gc_pending, gc_attempt_count, gc_retry_at_ms FROM blobs WHERE blob_id = ?1",
                [old.blob_id.as_bytes().as_slice()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
        };

        assert_eq!(pending()?, ("ready".into(), 1, 0, None));

        *PRESSURE.lock().unwrap() = Some(Pressure {
            sender: connection.owner.state.context.sender.clone(),
            permits: Vec::new(),
            armed: true,
            hit: false,
        });

        let operation = Arc::new(connection.owner.state.admit()?);

        let error = batch(Arc::clone(&operation), path.clone(), old.blob_id.shard())
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::StoreBusy);

        {
            let state = PRESSURE.lock().unwrap();
            let state = state.as_ref().unwrap();
            assert!(state.hit);
            assert_eq!(state.permits.len(), crate::connection::QUEUE_CAPACITY);
        }

        assert_eq!(pending()?, ("ready".into(), 1, 0, None));

        let cursor: i64 = sql.query_row(
            "SELECT cursor_row_id FROM maintenance_sweeps WHERE task = 'gc_blobs' AND shard = ?1",
            [i64::from(old.blob_id.shard())],
            |row| row.get(0),
        )?;

        assert_eq!(cursor, 0);

        let payload = path
            .join("objects")
            .join(format!("{:02x}", old.blob_id.shard()))
            .join(old.blob_id.to_string());

        assert_eq!(std::fs::read(&payload)?, b"predecessor bytes");

        {
            let mut state = PRESSURE.lock().unwrap();
            let state = state.as_mut().unwrap();
            state.armed = false;
            state.permits.clear();
        }

        batch(Arc::clone(&operation), path.clone(), old.blob_id.shard()).await?;
        assert!(!payload.exists());

        let count: i64 = sql.query_row(
            "SELECT count(*) FROM blobs WHERE blob_id = ?1",
            [old.blob_id.as_bytes().as_slice()],
            |row| row.get(0),
        )?;

        assert_eq!(count, 0);
        drop(operation);
        *PRESSURE.lock().unwrap() = None;
        let (_, mut body) = connection.get_object(read()).await?;
        assert_eq!(body.read_all(64).await?, b"acknowledged successor");
        sql.close().map_err(|(_, error)| error)?;
        connection.close().await?;
        let reopened = Connection::open(path, None).await?;
        let (_, mut body) = reopened.get_object(read()).await?;
        assert_eq!(body.read_all(64).await?, b"acknowledged successor");
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    writeln!(io::stdout().lock(), "PROBE VERIFIED")?;
    io::stdout().flush()?;
    let mut reply = String::new();
    io::stdin().lock().read_line(&mut reply)?;
    assert_eq!(reply, "exit\n");
    Ok(())
}

#[test]
fn scheduler_reports_real_checkpoint_refusal_and_preserves_acknowledged_object() -> TestResult {
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

        let before = connection.head_object(read()).await?;
        let denied = Arc::new(AtomicBool::new(true));
        let hits = Arc::new(AtomicUsize::new(0));
        let (reply, response) = oneshot::channel();
        let authorizer_denied = Arc::clone(&denied);
        let authorizer_hits = Arc::clone(&hits);

        connection
            .owner
            .state
            .context
            .sender
            .send(WorkerCommand::Probe {
                _state: Arc::clone(&connection.owner.state),
                work: Box::new(move |store| {
                    store
                        .test_checkpoint_authorizer(authorizer_denied, authorizer_hits)
                        .map_err(io::Error::other)
                }),
                reply,
            })
            .await
            .map_err(|_| "probe queue closed")?;

        response.await??;
        let receiver = connection.owner.state.context.diagnostics.receiver();
        pause(&connection, false);
        connection.owner.state.context.wake_maintenance.notify_one();

        let diagnostic = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((diagnostic, _)) = receiver.take()
                    && diagnostic.failure() == Some("io")
                {
                    return diagnostic;
                }

                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await?;

        pause(&connection, true);
        assert!(hits.load(Ordering::SeqCst) >= 1);
        assert_eq!(diagnostic.work(), Work::Cleanup);

        assert_eq!(
            Some(&diagnostic.store_id()),
            connection.owner.state.context.store_id.get()
        );

        denied.store(false, Ordering::SeqCst);
        let operation = Arc::new(connection.owner.state.admit()?);
        let (reply, response) = oneshot::channel();

        queue(
            &operation,
            Command::Checkpoint {
                reply,
                _operation: Arc::clone(&operation),
            },
        )?;

        response.await??;
        drop(operation);
        assert!(hits.load(Ordering::SeqCst) >= 2);

        assert_eq!(
            connection.head_object(read()).await?.blob_id,
            before.blob_id
        );

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
