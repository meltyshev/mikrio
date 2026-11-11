use super::objects::{put, read};
use super::*;
use crate::object::{Authority, BucketName, DeleteObjectsRequest, ObjectIdentifier, ObjectKey};
use crate::test_process as process;
use crate::{MutationOutcome, OperationFailure};
use std::io::BufRead as _;

mod interruption;

fn request(entries: &[(&str, Option<&str>)]) -> DeleteObjectsRequest {
    DeleteObjectsRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        expected_owner: None,
        objects: entries
            .iter()
            .map(|(key, etag)| ObjectIdentifier {
                key: ObjectKey::parse(key).unwrap(),
                etag: etag.map(str::to_owned),
            })
            .collect(),
    }
}

#[test]
fn batch_duplicates_keep_conditions_and_each_transaction_observes_prior_effects() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        let facts = connection
            .put_object(put("same"), b"old".as_slice())
            .await?;

        let etag = facts.etag();

        let result = connection
            .delete_objects(request(&[
                ("same", Some("\"wrong\"")),
                ("same", Some(&etag)),
                ("same", Some(&etag)),
                ("same", None),
                ("absent", Some("*")),
                ("absent", None),
            ]))
            .await?;

        assert_eq!(
            result.iter().map(|item| item.failure).collect::<Vec<_>>(),
            [
                Some(OperationFailure::PreconditionFailed),
                None,
                Some(OperationFailure::NoSuchKey),
                None,
                Some(OperationFailure::NoSuchKey),
                None,
            ]
        );

        assert_eq!(
            result
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            ["same", "same", "same", "same", "absent", "absent"]
        );

        for count in [0, 1001] {
            let mut batch = request(&[("same", None)]);
            batch.objects.resize(count, batch.objects[0].clone());
            let failure = connection.delete_objects(batch).await.unwrap_err();
            assert_eq!(failure.error.failure(), OperationFailure::InvalidArgument);
            assert_eq!(failure.error.outcome(), Some(MutationOutcome::NotCommitted));
            assert!(failure.items.is_none());
        }

        let mut full = request(&[("absent", None)]);
        full.objects.resize(1000, full.objects[0].clone());
        assert_eq!(connection.delete_objects(full).await?.len(), 1000);
        let mut wrong_owner = request(&[("same", None)]);
        wrong_owner.expected_owner = Some("000000000000".into());
        let failure = connection.delete_objects(wrong_owner).await.unwrap_err();
        assert_eq!(failure.error.failure(), OperationFailure::AccessDenied);
        assert!(failure.items.is_none());
        connection.close().await?;

        let failure = connection
            .delete_objects(request(&[("same", None)]))
            .await
            .unwrap_err();

        assert_eq!(failure.error.failure(), OperationFailure::StoreClosed);
        assert!(failure.items.is_none());
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

const CHILD: &str = "connection::tests::batch_deletion::batch_effect_child";
static OCCURRENCE: AtomicUsize = AtomicUsize::new(0);
static CANCELLATION: OnceLock<watch::Sender<bool>> = OnceLock::new();

fn checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let target = if mode == "rollback" {
        "object.delete.commit.before"
    } else {
        "object.delete.commit.after"
    };

    if point != target {
        return Ok(());
    }

    let occurrence = OCCURRENCE.fetch_add(1, Ordering::SeqCst);

    if mode == "revoke" && occurrence == 0 {
        let directory = std::env::var_os("mikrio_probe_directory")
            .ok_or_else(|| io::Error::other("missing probe directory"))?;

        let database =
            rusqlite::Connection::open(PathBuf::from(directory).join("metadata.sqlite3"))
                .map_err(io::Error::other)?;

        database
            .execute("UPDATE principals SET is_enabled=0", [])
            .map_err(io::Error::other)?;

        return Ok(());
    }

    if mode == "cancel" && occurrence == 0 {
        CANCELLATION
            .get()
            .ok_or_else(|| io::Error::other("missing cancellation"))?
            .send_replace(true);

        return Ok(());
    }

    if matches!(mode.as_str(), "rollback" | "unknown") && occurrence == 1 {
        return Err(io::Error::other("owned deletion settlement failure"));
    }

    Ok(())
}

#[test]
fn batch_settlement_preserves_unknown_and_undispatched_positions_across_reopen() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for mode in ["rollback", "unknown", "cancel", "revoke"] {
        let path = directory.path().join(mode);

        // Durable layout creation is fixture setup, outside the child's batch
        // settlement watchdog. The child still opens and owns its connection.
        OpenedStore::open(&path, None)?
            .close()
            .map_err(|(_, error)| error)?;

        let mut child = process::Probe::start(&path, mode, CHILD)?;
        assert_eq!(child.observation()?, "checked");
        child.finish()?;

        runtime()?.block_on(async {
            let connection = Connection::open(path, None).await?;

            for (index, key) in ["a", "b", "c"].iter().enumerate() {
                let exists = match mode {
                    "rollback" => index == 1,
                    "unknown" => index == 2,
                    _ => index > 0,
                };

                let result = connection.head_object(read(key)).await;
                assert_eq!(result.is_ok(), exists, "{mode}/{key}");

                if !exists {
                    assert_eq!(result.unwrap_err().failure(), OperationFailure::NoSuchKey);
                }
            }

            connection.close().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
    }

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for batch settlement effects"]
fn batch_effect_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing directory")?);

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        for key in ["a", "b", "c"] {
            connection
                .put_object(put(key), b"original".as_slice())
                .await?;
        }

        let mode = std::env::var("mikrio_probe_mode")?;

        let authority = if mode == "revoke" {
            let principal = connection
                .create_principal(crate::access::PrincipalName::new("batch-client".into())?)
                .await?;

            connection.put_principal_policy(
                principal.principal_id,
                r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:DeleteObject","Resource":"*"}}"#.into(),
            )
            .await?;

            let key = connection.create_access_key(principal.principal_id).await?;

            Authority::Http(Arc::new(
                connection
                    .resolve_application_key(key.access_key.access_key_id.as_str().into())
                    .await?
                    .verified(),
            ))
        } else {
            Authority::Native
        };

        let operation = Arc::new(connection.admit()?);

        CANCELLATION
            .set(operation.cancellation.clone())
            .map_err(|_| "duplicate cancellation hook")?;

        crate::protocol_fault::install(checkpoint)?;

        let result = super::super::deletion::delete_batch(
            operation,
            authority,
            request(&[("a", None), ("b", None), ("c", None)]),
        )
        .await;

        if mode == "rollback" || mode == "revoke" {
            let items = result?;

            assert_eq!(
                items.iter().map(|item| item.failure).collect::<Vec<_>>(),
                if mode == "rollback" {
                    [None, Some(OperationFailure::IoError), None]
                } else {
                    [
                        None,
                        Some(OperationFailure::AccessDenied),
                        Some(OperationFailure::AccessDenied),
                    ]
                }
            );
        } else {
            let failure = result.unwrap_err();
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
                    if mode == "unknown" {
                        MutationOutcome::Unknown
                    } else {
                        MutationOutcome::NotCommitted
                    },
                    MutationOutcome::NotCommitted,
                ]
            );

            assert_eq!(items[2].failure, None);
        }

        connection.close().await?;
        writeln!(io::stdout(), "PROBE checked")?;
        io::stdout().flush()?;
        let mut command = String::new();
        io::stdin().lock().read_line(&mut command)?;
        assert_eq!(command, "exit\n");
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
