use super::*;
use crate::GuardManager;
use std::sync::atomic::AtomicBool;

const CHILD: &str = "connection::tests::batch_deletion::interruption::batch_interruption_child";
static ENTERED: AtomicBool = AtomicBool::new(false);
static DELETIONS: AtomicUsize = AtomicUsize::new(0);

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;
    output.flush()
}

fn checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let target = if mode.ends_with("before") {
        "object.delete.commit.before"
    } else {
        "object.delete.commit.after"
    };

    if point != target || DELETIONS.fetch_add(1, Ordering::SeqCst) != 1 {
        return Ok(());
    }

    ENTERED.store(true, Ordering::SeqCst);
    announce("SECOND_ITEM")?;
    let mut command = String::new();
    io::stdin().lock().read_line(&mut command)?;

    if command != "exit\n" {
        return Err(io::Error::other("owned batch interruption control closed"));
    }

    Ok(())
}

fn interrupted_batch(mode: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    let path = directory.path().join(mode);

    // Prepare the durable layout before timing the actual in-flight batch
    // and close/cancellation barriers in the independently opening child.
    OpenedStore::open(&path, None)?
        .close()
        .map_err(|(_, error)| error)?;

    let mut child = process::Probe::start(&path, mode, CHILD)?;
    assert_eq!(child.observation()?, "SECOND_ITEM", "{mode}");
    assert_eq!(child.observation()?, "CLOSE_FENCED", "{mode}");

    // This process independently sees only committed rows while the actual
    // metadata worker in the child retains its transaction and admission.
    let database = rusqlite::Connection::open(path.join("metadata.sqlite3"))?;

    let keys: Vec<Vec<u8>> = database
        .prepare("SELECT key FROM objects ORDER BY key")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;

    let expected: Vec<Vec<u8>> = if mode.ends_with("before") {
        vec![b"b".to_vec(), b"c".to_vec()]
    } else {
        vec![b"c".to_vec()]
    };

    assert_eq!(keys, expected, "{mode}");
    database.close().map_err(|(_, error)| error)?;
    let guards = GuardManager::open(&path)?;

    assert!(
        guards
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none(),
        "{mode} released store use before native settlement"
    );

    child.finish()?;
    assert_eq!(child.observation()?, "SETTLED", "{mode}");

    drop(
        guards
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .ok_or("settled batch retained store use")?,
    );

    // Two independent opens prove the in-flight item settled and the next
    // item stayed untouched, including when its caller no longer exists.
    for _ in 0..2 {
        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;

            for key in ["a", "b"] {
                assert_eq!(
                    connection
                        .head_object(read(key))
                        .await
                        .unwrap_err()
                        .failure(),
                    OperationFailure::NoSuchKey,
                    "{mode}/{key}"
                );
            }

            let (_, mut body) = connection.get_object(read("c")).await?;
            assert_eq!(body.read_all(8).await?, b"original");
            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn close_before_waits_for_settlement_and_stops_dispatch() -> TestResult {
    interrupted_batch("close-before")
}

#[test]
fn close_after_waits_for_settlement_and_stops_dispatch() -> TestResult {
    interrupted_batch("close-after")
}

#[test]
fn signal_before_waits_for_settlement_and_stops_dispatch() -> TestResult {
    interrupted_batch("signal-before")
}

#[test]
fn signal_after_waits_for_settlement_and_stops_dispatch() -> TestResult {
    interrupted_batch("signal-after")
}

#[test]
fn drop_before_waits_for_settlement_and_stops_dispatch() -> TestResult {
    interrupted_batch("drop-before")
}

#[test]
fn drop_after_waits_for_settlement_and_stops_dispatch() -> TestResult {
    interrupted_batch("drop-after")
}

#[test]
#[ignore = "Owned subprocess entry for in-flight batch close and cancellation"]
fn batch_interruption_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing directory")?);

    let mode = std::env::var("mikrio_probe_mode")?;

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        for key in ["a", "b", "c"] {
            connection
                .put_object(put(key), b"original".as_slice())
                .await?;
        }

        crate::protocol_fault::install(checkpoint)?;
        let (cancel, canceled) = oneshot::channel();

        let mut batch = Box::pin(connection.delete_objects_until(
            request(&[("a", None), ("b", None), ("c", None)]),
            async {
                let _signal = canceled.await;
            },
        ));

        tokio::time::timeout(Duration::from_secs(10), async {
            while !ENTERED.load(Ordering::SeqCst) {
                std::future::poll_fn(|context| {
                    assert!(batch.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;

                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await?;

        if mode.starts_with("signal-") {
            cancel
                .send(())
                .map_err(|()| "batch signal receiver disappeared")?;

            // Poll the public cancellation future: delivery into core is now
            // observed before the close fence or native barrier is released.
            std::future::poll_fn(|context| {
                assert!(batch.as_mut().poll(context).is_pending());
                Poll::Ready(())
            })
            .await;
        }

        let batch = if mode.starts_with("drop-") {
            drop(batch);
            None
        } else {
            Some(batch)
        };

        let mut closing = Box::pin(connection.close());

        std::future::poll_fn(|context| {
            assert!(closing.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;

        drop(closing);

        assert_eq!(
            connection.owner.state.activity.lock().unwrap().operations,
            1
        );

        assert!(matches!(
            connection.admit(),
            Err(ConnectionError::StoreClosing)
        ));

        announce("CLOSE_FENCED")?;
        connection.close().await?;

        if let Some(batch) = batch {
            let failure = batch.await.unwrap_err();
            let items = failure.items.ok_or("missing indexed progress")?;

            assert_eq!(
                items
                    .iter()
                    .map(|item| item.key.as_str())
                    .collect::<Vec<_>>(),
                ["a", "b", "c"]
            );

            assert_eq!(
                items.iter().map(|item| item.outcome).collect::<Vec<_>>(),
                [
                    MutationOutcome::Committed,
                    MutationOutcome::Committed,
                    MutationOutcome::NotCommitted
                ]
            );

            assert!(items[..2].iter().all(|item| item.failure.is_none()));
            let expected = OperationFailure::OperationCanceled;
            assert_eq!(failure.error.failure(), expected);
            assert_eq!(items[2].failure, Some(expected));
        }

        announce("SETTLED")?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
