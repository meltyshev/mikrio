//! Order authorization changes against payload publication and metadata commit.

use std::io::BufRead as _;
use std::path::Path;

use super::objects::{put, read};
use super::*;
use crate::access::{AccessKeyId, PrincipalId, PrincipalName};
use crate::object::{BucketName, ObjectTags, ReadConditions, ReadConditionsInput};
use crate::test_process as process;
use crate::{BlobId, MutationOutcome, OperationFailure};

const CHILD: &str = "connection::tests::authorization_publication::authorization_process_child";
const POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject"],"Resource":"arn:aws:s3:::attachments/selected"}}"#;
const PREDECESSOR: &[u8] = b"protected predecessor bytes";
const CANDIDATE: &[u8] = b"complete replacement bytes";

fn announce(value: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE {value}")?;
    output.flush()
}

fn control() -> io::Result<()> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    if line != "exit\n" {
        return Err(io::Error::other("owned authorization control closed"));
    }

    Ok(())
}

fn checkpoint(point: &str) -> io::Result<()> {
    let mode = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    if mode == "revoke" {
        if point == "access_key.delete.begin.before" {
            announce("REVOCATION_ATTEMPT")?;
        }

        return Ok(());
    }

    if mode == "lost-ack" && point == "publication.commit.after" {
        return Err(io::Error::other("lost actual publication acknowledgment"));
    }

    let target = match mode.as_str() {
        "revoke-first" => "payload.published",
        "publish-first" => "publication.commit.before",
        "lost-ack" => "publication.witness.before",
        _ => return Err(io::Error::other("unknown authorization checkpoint")),
    };

    if point != target {
        return Ok(());
    }

    announce("PUBLICATION_PAUSED")?;
    control()
}

fn blob_path(path: &Path, blob: BlobId) -> PathBuf {
    path.join("objects")
        .join(format!("{:02x}", blob.shard()))
        .join(blob.to_string())
}

#[test]
fn process_revocation_orders_publication_and_lost_ack_without_invalidating_selected_readers()
-> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        for mode in ["revoke-first", "publish-first", "lost-ack"] {
            let scenario = directory.path().join(mode);
            std::fs::create_dir(&scenario)?;
            let path = scenario.join("store");
            let connection = Connection::open(path.clone(), None).await?;
            connection.pause_maintenance();

            connection
                .create_bucket(BucketName::parse("attachments")?)
                .await?;

            let mut seed = put("selected");
            seed.tags = Some(ObjectTags::new([("state".into(), "old".into())])?);
            connection.put_object(seed, PREDECESSOR).await?;

            let principal = connection
                .create_principal(PrincipalName::new("ordered-writer".into())?)
                .await?;

            connection
                .put_principal_policy(principal.principal_id, POLICY.into())
                .await?;

            let key = connection.create_access_key(principal.principal_id).await?;

            // Only nonsecret identities cross the owned process fixture file.
            std::fs::write(
                scenario.join("identities"),
                format!(
                    "{}\n{}\n",
                    principal.principal_id, key.access_key.access_key_id
                ),
            )?;

            let identity = Arc::new(
                connection
                    .resolve_application_key(key.access_key.access_key_id.as_str().into())
                    .await?
                    .verified(),
            );

            let (original, mut selected) = connection
                .get_object_http(Arc::clone(&identity), read("selected"))
                .await?;

            assert_eq!(selected.read_chunk(9).await?, Some(b"protected".to_vec()));
            let mut revoker = process::Probe::start(&scenario, "revoke", CHILD)?;
            assert_eq!(revoker.observation_async().await?, "REVOCATION_READY");
            let mut writer = process::Probe::start(&scenario, mode, CHILD)?;
            assert_eq!(writer.observation_async().await?, "PUBLICATION_PAUSED");

            let sql = rusqlite::Connection::open_with_flags(
                path.join("metadata.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;

            let candidate: Vec<u8> = sql.query_row(
                "SELECT blob_id FROM blobs WHERE blob_id != ?1",
                [original.blob_id.as_bytes().as_slice()],
                |row| row.get(0),
            )?;

            let candidate =
                BlobId::from_bytes(candidate.try_into().map_err(|_| "invalid candidate blob")?);

            assert_eq!(std::fs::read(blob_path(&path, candidate))?, CANDIDATE);
            let guards = GuardManager::open(&path)?;

            assert!(
                guards
                    .try_acquire(GuardTarget::Shard(candidate.shard()), GuardMode::Exclusive)?
                    .is_none(),
                "{mode}: writer lost its publication/witness guard"
            );

            std::fs::write(scenario.join("revoke"), b"start")?;
            assert_eq!(revoker.observation_async().await?, "REVOCATION_ATTEMPT");

            if mode == "publish-first" {
                // Publication holds SQLite's writer transaction. A
                // separate observer still sees the predecessor and live key.
                let keys: i64 =
                    sql.query_row("SELECT count(*) FROM access_keys", [], |row| row.get(0))?;

                assert_eq!(keys, 1);
                let current = connection.head_object(read("selected")).await?;
                assert_eq!(current.generation, original.generation);
                writer.finish_async().await?;
                assert_eq!(revoker.observation_async().await?, "REVOCATION_COMMITTED");
            } else {
                assert_eq!(revoker.observation_async().await?, "REVOCATION_COMMITTED");
                writer.finish_async().await?;
            }

            revoker.finish_async().await?;
            sql.close().map_err(|(_, error)| error)?;

            let current = connection.head_object(read("selected")).await?;

            let expected = if mode == "revoke-first" {
                assert_eq!(current.generation, original.generation);
                assert_eq!(current.blob_id, original.blob_id);
                assert_eq!(current.tags, original.tags);
                PREDECESSOR
            } else {
                assert_ne!(current.generation, original.generation);
                assert_eq!(current.blob_id, candidate);
                assert_eq!(current.tags, ObjectTags::default());
                CANDIDATE
            };

            // A retained verified identity cannot bypass current authority, even
            // when an unchanged validator would otherwise produce no body.
            let mut conditional = read("selected");

            conditional.conditions = ReadConditions::new(ReadConditionsInput {
                if_none_match: Some(current.etag.clone()),
                ..ReadConditionsInput::default()
            })?;

            assert_eq!(
                connection
                    .head_object_http(Arc::clone(&identity), conditional)
                    .await
                    .unwrap_err()
                    .failure(),
                OperationFailure::AccessDenied
            );

            super::super::maintenance::batch(
                Arc::new(connection.admit()?),
                path.clone(),
                original.blob_id.shard(),
            )
            .await?;

            assert_eq!(
                std::fs::read(blob_path(&path, original.blob_id))?,
                PREDECESSOR
            );

            assert_eq!(selected.read_all(100).await?, b" predecessor bytes");
            let (_, mut current_body) = connection.get_object(read("selected")).await?;
            assert_eq!(current_body.read_all(100).await?, expected);

            super::super::maintenance::batch(
                Arc::new(connection.admit()?),
                path.clone(),
                candidate.shard(),
            )
            .await?;

            assert_eq!(blob_path(&path, candidate).exists(), mode != "revoke-first");
            connection.close().await?;

            let reopened = Connection::open(path, None).await?;
            let (recovered, mut body) = reopened.get_object(read("selected")).await?;
            assert_eq!(recovered.generation, current.generation);
            assert_eq!(recovered.tags, current.tags);
            assert_eq!(body.read_all(100).await?, expected);

            assert!(
                reopened
                    .resolve_application_key(key.access_key.access_key_id.as_str().into())
                    .await
                    .is_err()
            );

            reopened.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned independent writer/revoker at actual authority and publication checkpoints"]
fn authorization_process_child() -> TestResult {
    let directory =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing directory")?);

    let mode = std::env::var("mikrio_probe_mode")?;
    let identities = std::fs::read_to_string(directory.join("identities"))?;
    let fields: Vec<_> = identities.lines().collect();
    assert_eq!(fields.len(), 2);
    let principal = PrincipalId::parse(fields[0])?;
    let key = AccessKeyId::parse(fields[1].into())?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.join("store"), None).await?;
        connection.pause_maintenance();

        if mode == "revoke" {
            // Open before the writer takes its transaction, so initialization
            // cannot be mistaken for the observed revocation attempt.
            announce("REVOCATION_READY")?;

            tokio::time::timeout(Duration::from_secs(10), async {
                while !directory.join("revoke").exists() {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await?;

            crate::protocol_fault::install(checkpoint)?;
            connection.delete_access_key(principal, key).await?;
            announce("REVOCATION_COMMITTED")?;
            control()?;
            connection.close().await?;
            return Ok(());
        }

        let identity = Arc::new(
            connection
                .resolve_application_key(key.as_str().into())
                .await?
                .verified(),
        );

        let lease = connection.reserve_listener()?;
        lease.ready()?;
        crate::protocol_fault::install(checkpoint)?;

        let result = lease
            .connection()
            .put_object_http(identity, put("selected"), CANDIDATE, || {
                Ok(crate::integrity::ExpectedDigests::default())
            })
            .await;

        if mode == "revoke-first" {
            let error = result.unwrap_err();
            assert_eq!(error.failure(), OperationFailure::AccessDenied);
            assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        } else {
            assert_eq!(result?.length(), u64::try_from(CANDIDATE.len())?);
        }

        lease.finish().await?;
        drop(lease);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
