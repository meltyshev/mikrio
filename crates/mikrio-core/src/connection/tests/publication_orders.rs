//! Independent writers order conditions and ambiguous outcomes at publication.

use super::objects::{put, read};
use super::*;
use crate::OperationFailure;
use crate::object::BucketName;
use crate::test_process as process;

const WRITER: &str = "connection::tests::faults::publication_fault_child";

#[test]
fn two_create_only_writers_at_publication_allow_exactly_one_commit_in_either_order() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        for first in 0..2 {
            let path = directory.path().join(format!("store-{first}"));
            let connection = Connection::open(path.clone(), None).await?;

            connection
                .create_bucket(BucketName::parse("attachments")?)
                .await?;

            let mut writers = [
                process::Probe::start(&path, "create-only:commit_fence.entered", WRITER)?,
                process::Probe::start(&path, "create-only:commit_fence.entered", WRITER)?,
            ];

            for writer in &writers {
                assert_eq!(writer.observation_async().await?, "CHECKPOINT");
            }

            writers[first].command("exit")?;
            assert_eq!(writers[first].observation_async().await?, "Committed");
            let winner = connection.head_object(read("selected")).await?;
            writers[1 - first].command("exit")?;

            assert_eq!(
                writers[1 - first].observation_async().await?,
                "PreconditionFailed"
            );

            for writer in &mut writers {
                writer.finish_async().await?;
            }

            let current = connection.head_object(read("selected")).await?;
            assert_eq!(current.generation, winner.generation);
            assert_eq!(current.blob_id, winner.blob_id);
            super::faults::publication_references(&path)?;
            connection.close().await?;

            let reopened = Connection::open(path, None).await?;
            let (selected, mut body) = reopened.get_object(read("selected")).await?;
            assert_eq!(selected.generation, winner.generation);
            assert_eq!(body.read_all(64).await?, b"replacement bytes");
            body.close().await?;
            reopened.close().await?;
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn conditional_put_rechecks_the_peer_generation_at_publication() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("selected"), b"original".as_slice())
            .await?;

        let mut writer =
            process::Probe::start(&path, "match-current:commit_fence.entered", WRITER)?;
        assert_eq!(writer.observation_async().await?, "CHECKPOINT");

        connection
            .put_object(put("selected"), b"peer winner".as_slice())
            .await?;

        let winner = super::faults::object_state(&path, "selected")?;
        writer.command("exit")?;
        assert_eq!(writer.observation_async().await?, "PreconditionFailed");
        writer.finish_async().await?;
        assert_eq!(super::faults::object_state(&path, "selected")?, winner);
        connection.close().await?;

        let reopened = Connection::open(path.clone(), None).await?;
        let (_, mut body) = reopened.get_object(read("selected")).await?;
        assert_eq!(body.read_all(64).await?, b"peer winner");
        body.close().await?;
        assert_eq!(super::faults::object_state(&path, "selected")?, winner);
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn put_and_delete_commit_orders_preserve_selected_bytes_after_unknown_delivery() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        for delete_first in [true, false] {
            let path = directory.path().join(format!("store-{delete_first}"));
            let connection = Connection::open(path.clone(), None).await?;

            connection
                .create_bucket(BucketName::parse("attachments")?)
                .await?;

            connection
                .put_object(put("selected"), b"original".as_slice())
                .await?;

            let (_, mut original) = connection.get_object(read("selected")).await?;
            let mut writer =
                process::Probe::start(&path, "ordered-unknown:commit_fence.entered", WRITER)?;
            assert_eq!(writer.observation_async().await?, "CHECKPOINT");

            if delete_first {
                connection.delete_object(read("selected"), None).await?;
            }

            writer.finish_async().await?;
            assert_eq!(writer.observation_async().await?, "Unknown");

            if !delete_first {
                connection.delete_object(read("selected"), None).await?;
            }

            assert_eq!(original.read_all(64).await?, b"original");
            original.close().await?;
            super::faults::publication_references(&path)?;
            connection.close().await?;

            for _ in 0..2 {
                let reopened = Connection::open(path.clone(), None).await?;
                let selected = reopened.get_object(read("selected")).await;

                if delete_first {
                    let (_, mut body) = selected?;
                    assert_eq!(body.read_all(64).await?, b"replacement bytes");
                    body.close().await?;
                } else {
                    assert_eq!(selected.unwrap_err().failure(), OperationFailure::NoSuchKey);
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
fn conditional_delete_and_replacement_observe_both_commit_orders() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        for (index, delete_first, wildcard) in [
            (0, true, false),
            (1, false, false),
            (2, true, true),
            (3, false, true),
        ] {
            let path = directory.path().join(format!("store-{index}"));
            let connection = Connection::open(path.clone(), None).await?;

            connection
                .create_bucket(BucketName::parse("attachments")?)
                .await?;

            connection
                .put_object(put("selected"), b"original".as_slice())
                .await?;

            let (selected, mut original) = connection.get_object(read("selected")).await?;

            let condition = if wildcard {
                "*".to_owned()
            } else {
                selected.etag.clone()
            };

            let mut writer =
                process::Probe::start(&path, "ordered-unknown:commit_fence.entered", WRITER)?;
            assert_eq!(writer.observation_async().await?, "CHECKPOINT");

            if delete_first {
                connection
                    .delete_object(read("selected"), Some(condition.clone()))
                    .await?;
            }

            writer.finish_async().await?;
            assert_eq!(writer.observation_async().await?, "Unknown");
            let replacement = connection.head_object(read("selected")).await?;
            assert_ne!(replacement.etag, selected.etag);

            if !delete_first {
                let deleted = connection
                    .delete_object(read("selected"), Some(condition))
                    .await;

                if wildcard {
                    deleted?;
                } else {
                    let error = deleted.unwrap_err();
                    assert_eq!(error.failure(), OperationFailure::PreconditionFailed);
                    assert_eq!(error.outcome(), Some(crate::MutationOutcome::NotCommitted));

                    assert_eq!(
                        connection.head_object(read("selected")).await?.generation,
                        replacement.generation
                    );
                }
            }

            assert_eq!(original.read_all(64).await?, b"original");
            original.close().await?;
            super::faults::publication_references(&path)?;
            connection.close().await?;

            for _ in 0..2 {
                let reopened = Connection::open(path.clone(), None).await?;
                let current = reopened.get_object(read("selected")).await;

                if delete_first || !wildcard {
                    let (stored, mut body) = current?;
                    assert_eq!(stored.generation, replacement.generation);
                    assert_eq!(body.read_all(64).await?, b"replacement bytes");
                    body.close().await?;
                } else {
                    assert_eq!(current.unwrap_err().failure(), OperationFailure::NoSuchKey);
                }

                reopened.close().await?;
            }
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
