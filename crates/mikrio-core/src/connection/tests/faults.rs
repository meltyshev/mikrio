//! Write interruption and I/O failures with subprocess barriers.

use std::sync::atomic::AtomicBool;

use super::objects::{put, read};
use super::*;
use crate::OperationFailure;
use crate::object::BucketName;
use crate::test_process as process;

mod effects;

const CHILD: &str = "connection::tests::faults::publication_fault_child";
static FIRED: AtomicBool = AtomicBool::new(false);

fn checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    let (kind, target) = mode
        .split_once(':')
        .ok_or_else(|| io::Error::other("missing fault mode"))?;

    if kind == "ordered-unknown" && point == target {
        announce("CHECKPOINT")?;
        return process::read_exit();
    }

    if matches!(kind, "unavailable" | "witness" | "ordered-unknown") {
        if point == "publication.commit.after" {
            FIRED.store(true, Ordering::SeqCst);
            return Err(io::Error::other("lost actual publication acknowledgment"));
        }

        let witness = if kind == "ordered-unknown" {
            "publication.witness.before"
        } else {
            target
        };

        if point != witness {
            return Ok(());
        }

        if kind == "ordered-unknown" {
            return Err(io::Error::other("authoritative witness read unavailable"));
        }

        announce("CHECKPOINT")?;
        process::read_exit()?;

        return if kind == "unavailable" {
            Err(io::Error::other("authoritative witness read unavailable"))
        } else {
            Ok(())
        };
    }

    if target != point || FIRED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    if kind == "error" {
        return Err(io::Error::other("injected effect acknowledgment failure"));
    }

    announce("CHECKPOINT")?;
    process::read_exit()
}

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;
    output.flush()
}

pub(super) fn object_state(
    path: &std::path::Path,
    key: &str,
) -> Result<Vec<rusqlite::types::Value>, Box<dyn std::error::Error>> {
    publication_references(path)?;

    let database = rusqlite::Connection::open_with_flags(
        path.join("metadata.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    assert!(!database.prepare("PRAGMA foreign_key_check")?.exists([])?);

    let state = database.query_row(
        "SELECT o.row_id, o.bucket_id, o.key, o.generation, o.blob_id,
                o.content_type, o.content_encoding, o.content_disposition,
                o.content_language, o.cache_control, o.expires_at_ms,
                o.user_metadata_json, o.tags_json, o.last_modified_at_ms,
                o.lifecycle_anchor_at_ms,
                b.row_id, b.blob_id, b.shard, b.state, b.write_kind,
                b.target_bucket_id, b.target_key, b.target_upload_id,
                b.target_part_number, b.byte_length, b.part_count, b.etag,
                b.md5_digest, b.checksum_type, b.checksums_json, b.created_at_ms,
                b.gc_pending, b.gc_retry_at_ms, b.gc_attempt_count, b.gc_error_code
         FROM objects AS o JOIN blobs AS b ON o.blob_id = b.blob_id
         WHERE o.key = ?1",
        [key.as_bytes()],
        |row| {
            (0..row.as_ref().column_count())
                .map(|index| row.get(index))
                .collect()
        },
    )?;

    database.close().map_err(|(_, error)| error)?;
    Ok(state)
}

pub(super) fn publication_references(path: &std::path::Path) -> TestResult {
    let database = rusqlite::Connection::open_with_flags(
        path.join("metadata.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    assert!(!database.prepare("PRAGMA foreign_key_check")?.exists([])?);

    assert!(
        !database
            .prepare(
                "SELECT 1 FROM objects AS o LEFT JOIN blobs AS b ON b.blob_id = o.blob_id
                 WHERE b.state IS NULL OR b.state <> 'ready' OR b.gc_pending <> 0
                 UNION ALL
                 SELECT 1 FROM multipart_parts AS p LEFT JOIN blobs AS b ON b.blob_id = p.blob_id
                 WHERE b.state IS NULL OR b.state <> 'ready' OR b.gc_pending <> 0
                 UNION ALL
                 SELECT 1 FROM blobs AS b WHERE b.state = 'ready' AND b.gc_pending = 0
                 AND NOT EXISTS (SELECT 1 FROM objects AS o WHERE o.blob_id = b.blob_id)
                 AND NOT EXISTS (SELECT 1 FROM multipart_parts AS p WHERE p.blob_id = b.blob_id)",
            )?
            .exists([])?,
        "publication retained provisional reference or retirement state"
    );

    database.close().map_err(|(_, error)| error)?;
    Ok(())
}

fn kill_publications(points: &[&str]) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        for (index, point) in points.iter().enumerate() {
            let path = directory.path().join(format!("store-{index}"));
            let connection = Connection::open(path.clone(), None).await?;

            connection
                .create_bucket(BucketName::parse("attachments")?)
                .await?;

            connection
                .put_object(put("selected"), b"predecessor".as_slice())
                .await?;

            let original = object_state(&path, "selected")?;
            let mut child = process::Probe::start(&path, &format!("kill:{point}"), CHILD)?;
            assert_eq!(child.observation_async().await?, "CHECKPOINT", "{point}");

            let expected: &[u8] = if *point == "publication.commit.after" {
                b"replacement bytes"
            } else {
                b"predecessor"
            };

            let (_, mut body) = connection.get_object(read("selected")).await?;
            assert_eq!(body.read_all(100).await?, expected, "{point}");
            child.kill_and_wait_async().await?;
            connection.close().await?;

            for _ in 0..2 {
                let reopened = Connection::open(path.clone(), None).await?;
                let (_, mut body) = reopened.get_object(read("selected")).await?;
                assert_eq!(body.read_all(100).await?, expected, "{point}");

                if *point != "publication.commit.after" {
                    assert_eq!(object_state(&path, "selected")?, original, "{point}");
                }

                reopened
                    .put_object(put("after-recovery"), b"new write".as_slice())
                    .await?;

                reopened.close().await?;
            }
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

fn publication_error(point: &str) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"predecessor".as_slice())
            .await?;

        let original = object_state(&path, "selected")?;
        let mut child = process::Probe::start(&path, &format!("error:{point}"), CHILD)?;
        let committed = point == "publication.commit.after";

        assert_eq!(
            child.observation_async().await?,
            if committed {
                "Committed"
            } else {
                "NotCommitted"
            },
            "{point}"
        );

        let (_, mut body) = connection.get_object(read("selected")).await?;

        let expected: &[u8] = if committed {
            b"replacement bytes"
        } else {
            b"predecessor"
        };

        assert_eq!(body.read_all(100).await?, expected, "{point}");
        child.finish_async().await?;
        connection.close().await?;

        let reopened = Connection::open(path.clone(), None).await?;
        let (_, mut body) = reopened.get_object(read("selected")).await?;
        assert_eq!(body.read_all(100).await?, expected, "{point}");

        if !committed {
            assert_eq!(object_state(&path, "selected")?, original, "{point}");
        }

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[derive(Clone, Copy)]
enum WitnessChange {
    None,
    Replace,
    Retag,
}

#[test]
fn protected_publication_witness_preserves_unknown_reads_and_survives_later_mutations() -> TestResult
{
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        for (index, (mode, change)) in [
            (
                "unavailable:publication.witness.before",
                WitnessChange::None,
            ),
            ("unavailable:publication.witness.after", WitnessChange::None),
            ("witness:publication.witness.before", WitnessChange::Replace),
            ("witness:publication.witness.before", WitnessChange::Retag),
        ]
        .into_iter()
        .enumerate()
        {
            let path = directory.path().join(format!("store-{index}"));
            let connection = Connection::open(path.clone(), None).await?;

            connection
                .create_bucket(BucketName::parse("attachments")?)
                .await?;

            connection
                .put_object(put("selected"), b"predecessor".as_slice())
                .await?;

            let mut child = process::Probe::start(&path, mode, CHILD)?;
            assert_eq!(child.observation_async().await?, "CHECKPOINT");

            // The actual commit has completed, but its reply and fresh witness
            // read have not. A distinct process observes only committed state.
            let committed = connection.head_object(read("selected")).await?;

            assert_eq!(
                committed.tags,
                crate::object::ObjectTags::new([("status".into(), "initial".into())])?
            );

            let candidate = committed.blob_id;

            let candidate_path = path
                .join("objects")
                .join(format!("{:02x}", candidate.shard()))
                .join(candidate.to_string());

            let guards = GuardManager::open(&path)?;

            assert!(
                guards
                    .try_acquire(GuardTarget::Shard(candidate.shard()), GuardMode::Exclusive)?
                    .is_none()
            );

            assert_eq!(std::fs::read(&candidate_path)?, b"replacement bytes");
            let tags = crate::object::ObjectTags::new([("status".into(), "retagged".into())])?;

            match change {
                WitnessChange::None => {}
                WitnessChange::Replace => {
                    connection
                        .put_object(put("selected"), b"later bytes".as_slice())
                        .await?;
                }
                WitnessChange::Retag => {
                    connection
                        .replace_object_tags(read("selected"), tags.clone())
                        .await?;
                }
            }

            assert!(
                guards
                    .try_acquire(GuardTarget::Shard(candidate.shard()), GuardMode::Exclusive)?
                    .is_none()
            );

            assert_eq!(std::fs::read(&candidate_path)?, b"replacement bytes");

            child.finish_async().await?;

            assert_eq!(
                child.observation_async().await?,
                if mode.starts_with("unavailable:") {
                    "Unknown"
                } else {
                    "Committed"
                }
            );

            let expected: &[u8] = if matches!(change, WitnessChange::Replace) {
                b"later bytes"
            } else {
                b"replacement bytes"
            };

            let (selected, mut body) = connection.get_object(read("selected")).await?;
            assert_eq!(body.read_all(100).await?, expected);
            body.close().await?;

            if matches!(change, WitnessChange::Retag) {
                assert_ne!(selected.generation, committed.generation);
                assert_eq!(selected.tags, tags);
                assert_eq!(selected.blob_id, candidate);
                assert_eq!(selected.etag, committed.etag);
                assert_eq!(selected.last_modified, committed.last_modified);
            }

            connection.close().await?;

            for _ in 0..2 {
                let reopened = Connection::open(path.clone(), None).await?;
                let (selected, mut body) = reopened.get_object(read("selected")).await?;
                assert_eq!(body.read_all(100).await?, expected);
                body.close().await?;

                if matches!(change, WitnessChange::Retag) {
                    assert_eq!(selected.tags, tags);
                    assert_eq!(selected.blob_id, candidate);
                    assert_eq!(selected.etag, committed.etag);
                    assert_eq!(selected.last_modified, committed.last_modified);
                }

                reopened.close().await?;
            }
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for actual publication effects"]
fn publication_fault_child() -> TestResult {
    let path =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?);

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        let mode = std::env::var("mikrio_probe_mode")?;
        let mut request = put("selected");

        if mode.starts_with("witness:") || mode.starts_with("unavailable:") {
            request.tags = Some(crate::object::ObjectTags::new([(
                "status".into(),
                "initial".into(),
            )])?);
        }

        if mode.starts_with("create-only:") {
            request.conditions = crate::object::WriteConditions::new(None, Some("*".into()))?;
        }

        if mode.starts_with("match-current:") {
            let original = connection.head_object(read("selected")).await?;
            request.conditions = crate::object::WriteConditions::new(Some(original.etag), None)?;
        }

        crate::protocol_fault::install(checkpoint)?;

        let result = connection
            .put_object(request, b"replacement bytes".as_slice())
            .await;

        if mode.starts_with("unavailable:") || mode.starts_with("ordered-unknown:") {
            assert!(FIRED.load(Ordering::SeqCst));
            let error = result.unwrap_err();
            assert_eq!(error.outcome(), Some(crate::MutationOutcome::Unknown));
            announce("Unknown")?;
        } else if mode.starts_with("create-only:") || mode.starts_with("match-current:") {
            assert!(FIRED.load(Ordering::SeqCst));

            match result {
                Ok(_) => announce("Committed")?,
                Err(error) => {
                    assert_eq!(error.failure(), OperationFailure::PreconditionFailed);
                    assert_eq!(error.outcome(), Some(crate::MutationOutcome::NotCommitted));
                    announce("PreconditionFailed")?;
                }
            }

            process::wait_for_exit().await?;
        } else if mode.starts_with("witness:") {
            assert!(FIRED.load(Ordering::SeqCst));
            result?;
            announce("Committed")?;
        } else if mode.starts_with("error:") {
            assert!(FIRED.load(Ordering::SeqCst));

            match result {
                Ok(_) => announce("Committed")?,
                Err(error) => {
                    assert_eq!(error.outcome(), Some(crate::MutationOutcome::NotCommitted));
                    announce("NotCommitted")?;
                }
            }

            process::wait_for_exit().await?;
        } else {
            result?;
        }

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
