//! Test-binary entry for externally recorded block-device prefixes. The guest
//! controller owns marks, independent expected facts and process termination.

use std::fmt::Write as _;
use std::io::{BufRead as _, Read as _};
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::objects::{put, read};
use super::*;
use crate::object::{BucketName, UserMetadata};
use crate::{BlobId, OperationFailure};

const PREDECESSOR: &[u8] = &[b'A'; 8192];
const REPLACEMENT: &[u8] = &[b'B'; 16384];
static ORDINAL: AtomicUsize = AtomicUsize::new(0);

fn announce(kind: &str, value: &Value) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "REPLAY_{kind} {value}")?;
    output.flush()
}

fn checkpoint(point: &str) -> io::Result<()> {
    let ordinal = ORDINAL.fetch_add(1, Ordering::SeqCst);
    announce("EVENT", &json!({"point": point, "ordinal": ordinal}))?;
    let mut line = String::new();
    io::stdin().lock().take(16).read_line(&mut line)?;

    if line != "continue\n" {
        return Err(io::Error::other("owned replay control pipe closed"));
    }

    Ok(())
}

fn initialize(path: &Path, trace: bool) -> TestResult {
    let _scope = trace.then(|| crate::initialize::fault::install(checkpoint));

    let store = match OpenedStore::open(path, Some("eu-west-1")) {
        Ok(store) => store,
        Err(error)
            if !trace
                && matches!(
                    error.failure(),
                    crate::OpenFailure::InvalidStoreDirectory | crate::OpenFailure::StoreCorrupt
                ) =>
        {
            announce(
                "REFUSED",
                &json!({"failure": format!("{:?}", error.failure())}),
            )?;

            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };

    announce(
        "STATE",
        &json!({"store_id": store.record().store_id.to_string()}),
    )?;

    if trace {
        checkpoint("ack.initialize")?;
    }

    store.close().map_err(|(_, error)| error)?;
    Ok(())
}

async fn prepare(connection: &Connection, garbage: bool) -> TestResult {
    connection
        .create_bucket(BucketName::parse("attachments")?)
        .await?;

    let mut request = put("selected");
    request.metadata = UserMetadata::new([("version".into(), "old".into())])?;
    connection.put_object(request, PREDECESSOR).await?;

    if !garbage {
        return Ok(());
    }

    connection
        .put_object(put("orphan"), b"retired bytes".as_slice())
        .await?;

    let orphan = connection.head_object(read("orphan")).await?.blob_id;
    connection.delete_object(read("orphan"), None).await?;
    announce("ORPHAN", &json!({"blob_id": orphan.to_string()}))?;
    Ok(())
}

async fn observe(connection: &Connection) -> TestResult {
    let (selected, mut body) = connection.get_object(read("selected")).await?;
    let payload = body.read_all(16 * 1024).await?;

    let version = if payload == PREDECESSOR {
        "old"
    } else if payload == REPLACEMENT {
        "new"
    } else {
        return Err("recovered partial or unknown payload".into());
    };

    assert_eq!(selected.length.get(), u64::try_from(payload.len())?);

    assert_eq!(
        selected.metadata,
        UserMetadata::new([("version".into(), version.into())])?
    );

    let mut digest = String::with_capacity(64);

    for byte in Sha256::digest(&payload) {
        write!(digest, "{byte:02x}")?;
    }

    announce(
        "STATE",
        &json!({
            "store_id": connection.info().record.store_id.to_string(),
            "generation": selected.generation,
            "blob_id": selected.blob_id.to_string(),
            "length": selected.length.get(),
            "sha256": digest,
            "etag": selected.etag,
            "metadata": selected.metadata.entries(),
            "version": version,
        }),
    )?;

    Ok(())
}

async fn collect(connection: &Connection, path: &Path, orphan: BlobId) -> TestResult {
    super::super::maintenance::batch(
        Arc::new(connection.admit()?),
        path.to_path_buf(),
        orphan.shard(),
    )
    .await?;

    Ok(())
}

async fn verify_collection(connection: &Connection, path: &Path, orphan: BlobId) -> TestResult {
    assert_eq!(
        connection
            .head_object(read("orphan"))
            .await
            .unwrap_err()
            .failure(),
        OperationFailure::NoSuchKey
    );

    collect(connection, path, orphan).await?;

    for tree in ["staging", "objects"] {
        assert!(
            !path
                .join(tree)
                .join(format!("{:02x}", orphan.shard()))
                .join(orphan.to_string())
                .try_exists()?
        );
    }

    let sql = rusqlite::Connection::open_with_flags(
        path.join("metadata.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    let count: i64 = sql.query_row(
        "SELECT count(*) FROM blobs WHERE blob_id = ?1",
        [orphan.as_bytes().as_slice()],
        |row| row.get(0),
    )?;

    assert_eq!(count, 0);
    announce("COLLECTED", &json!({"blob_id": orphan.to_string()}))?;
    Ok(())
}

#[test]
#[ignore = "Owned Linux guest entry; requires an external block replay controller"]
fn storage_replay_child() -> TestResult {
    let path = PathBuf::from(
        std::env::var_os("mikrio_replay_directory").ok_or("missing owned replay directory")?,
    );

    let mode = std::env::var("mikrio_replay_mode")?;

    if matches!(mode.as_str(), "initialize" | "verify-initialize") {
        return initialize(&path, mode == "initialize");
    }

    if !matches!(
        mode.as_str(),
        "prepare-write" | "prepare-gc" | "write" | "gc" | "verify-write" | "verify-gc" | "probe"
    ) {
        return Err("unknown replay child mode".into());
    }

    let orphan = std::env::var("mikrio_replay_orphan")
        .ok()
        .map(|value| BlobId::parse(&value))
        .transpose()?;

    runtime()?.block_on(async {
        // Keep an existing orphan alive until the context's automatic scheduler
        // is paused. Only the actual production batch is driven manually below.
        let protection = if let Some(orphan) = orphan {
            let manager = GuardManager::open(&path)?;

            let store = manager
                .try_acquire(GuardTarget::Store, GuardMode::Shared)?
                .ok_or("replay store protection unavailable")?;

            let shard = manager
                .try_acquire(GuardTarget::Shard(orphan.shard()), GuardMode::Shared)?
                .ok_or("replay orphan protection unavailable")?;

            Some((shard, store))
        } else {
            None
        };

        let connection = Connection::open(path.clone(), Some("eu-west-1".into())).await?;
        connection.pause_maintenance();
        drop(protection);

        match mode.as_str() {
            "prepare-write" | "prepare-gc" => prepare(&connection, mode == "prepare-gc").await?,
            "write" => {
                crate::protocol_fault::install(checkpoint)?;
                let mut request = put("selected");
                request.metadata = UserMetadata::new([("version".into(), "new".into())])?;
                connection.put_object(request, REPLACEMENT).await?;
                checkpoint("ack.write")?;
            }
            "gc" => {
                crate::protocol_fault::install(checkpoint)?;
                collect(&connection, &path, orphan.ok_or("missing orphan identity")?).await?;
                checkpoint("ack.gc")?;
            }
            "verify-gc" => {
                verify_collection(&connection, &path, orphan.ok_or("missing orphan identity")?)
                    .await?;
            }
            "probe" => {
                let bucket = BucketName::parse("after-recovery")?;
                connection.create_bucket(bucket.clone()).await?;

                assert_eq!(
                    connection.head_bucket(bucket.clone(), None).await?,
                    "eu-west-1"
                );

                connection.delete_bucket(bucket, None).await?;
                connection.close().await?;
                return Ok(());
            }
            "verify-write" => {}
            _ => return Err("unknown replay child mode".into()),
        }

        observe(&connection).await?;
        connection.close().await?;
        Ok(())
    })
}
