//! Generated SQL histories; physical guards, payload I/O and process death have
//! separate connection fixtures. The model never calls the implementation.

use std::io::Write as _;

use super::*;
use crate::multipart::CompletionSelection;
use rusqlite::OptionalExtension as _;

mod concurrent;
mod model;
use model::{Action, Model, Outcome, Upload};

const ACTIONS: [Action; 8] = [
    Action::Select,
    Action::Replace,
    Action::Claim(60),
    Action::Claim(62),
    Action::Complete(60),
    Action::Abort,
    Action::Recover(60),
    Action::Reopen,
];

#[allow(
    clippy::panic,
    reason = "an unmodeled engine outcome fails the fixture"
)]
fn observed(result: Result<Outcome, crate::OperationError>, action: Action) -> Outcome {
    let error = match result {
        Ok(outcome) => return outcome,
        Err(error) => error,
    };

    assert_eq!(
        error.outcome(),
        (action != Action::Select).then_some(MutationOutcome::NotCommitted)
    );

    match error.failure() {
        OperationFailure::NoSuchUpload => Outcome::NoSuchUpload,
        OperationFailure::InvalidPart => Outcome::InvalidPart,
        OperationFailure::OperationAborted => Outcome::OperationAborted,
        other => panic!("unexpected model outcome: {other:?}: {error}"),
    }
}

#[allow(
    clippy::unreachable,
    reason = "the driver handles connection replacement before dispatch"
)]
fn invoke(
    database: &mut MetadataDatabase,
    request: &CompleteRequest,
    selection: &mut CompletionSelection,
    facts: &PayloadFacts,
    action: Action,
    step: u8,
) -> Result<Outcome, crate::OperationError> {
    match action {
        Action::Select => {
            *selection = database.select_completion(&Authority::Native, request)?;
        }
        Action::Replace => {
            let mut input = super::super::parts::part(request.upload.upload_id.clone());
            input.part_number = PartNumber::new(1).unwrap();
            let (reserved, blob) = super::super::parts::reserve(database, &input, 100 + step)?;
            database.publish_part(&Authority::Native, &input, reserved, blob, facts)?;
        }
        Action::Claim(owner) => {
            assert!(database.claim_completion(
                &Authority::Native,
                request,
                selection,
                claim(owner)
            )?);
        }
        Action::Complete(owner) => {
            let result = database.publish_completion(
                &Authority::Native,
                request,
                selection,
                claim(owner),
                facts,
            )?;

            assert_eq!(result.length.get(), 3);
            assert_eq!(result.etag, "\"af5da9f45af7a300e3aded972f8ff687-1\"");

            assert_eq!(
                result.checksum.encode(),
                "T4tCwi3TcptRm6b2jS2nzFstYG0F2u1a1RKMwD5sY1g=-1"
            );
        }
        Action::Recover(owner) => {
            let output = claim(owner).blob_id;
            let collected = database.gc_mark(output)?;

            if collected {
                database.gc_finish(output)?;
            }

            return Ok(Outcome::Collected(collected));
        }
        Action::Abort => database.abort_multipart_upload(&Authority::Native, &request.upload)?,
        Action::Reopen => unreachable!("connection ownership is handled by the history driver"),
    }

    Ok(Outcome::Success)
}

fn assert_state(database: &MetadataDatabase, model: &Model, generations: &[Vec<u8>]) -> TestResult {
    let upload: Option<(String, Option<Vec<u8>>)> = database
        .connection
        .query_row(
            "SELECT state, completion_claim_id FROM multipart_uploads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let expected = match model.upload {
        Upload::Open => Some(("open".into(), None)),
        Upload::Completing(owner) => Some(("completing".into(), Some(vec![owner; 16]))),
        Upload::Aborted | Upload::Completed(_) => None,
    };

    assert_eq!(upload, expected);
    assert_eq!(count(database)?, i64::from(expected.is_some()));

    let part: Option<Vec<u8>> = database
        .connection
        .query_row("SELECT generation FROM multipart_parts", [], |row| {
            row.get(0)
        })
        .optional()?;

    assert_eq!(
        part.as_ref(),
        expected
            .is_some()
            .then(|| &generations[usize::from(model.part_generation)])
    );

    let receipt: Option<String> = database
        .connection
        .query_row("SELECT outcome FROM multipart_receipts", [], |row| {
            row.get(0)
        })
        .optional()?;

    assert_eq!(
        receipt.as_deref(),
        match model.upload {
            Upload::Aborted => Some("aborted"),
            Upload::Completed(_) => Some("completed"),
            _ => None,
        }
    );

    let object: Option<Vec<u8>> = database
        .connection
        .query_row("SELECT blob_id FROM objects", [], |row| row.get(0))
        .optional()?;

    assert_eq!(
        object,
        match model.upload {
            Upload::Completed(owner) => Some(claim(owner).blob_id.as_bytes().to_vec()),
            _ => None,
        }
    );

    let output_count: i64 = database.connection.query_row(
        "SELECT count(*) FROM blobs WHERE write_kind = 'complete_multipart_upload'",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(output_count, i64::try_from(model.reserved.len())?);

    for owner in &model.reserved {
        let (state, pending): (String, bool) = database.connection.query_row(
            "SELECT state, gc_pending FROM blobs WHERE blob_id = ?1",
            [claim(*owner).blob_id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let completed = model.upload == Upload::Completed(*owner);
        assert_eq!(state, if completed { "ready" } else { "reserved" });
        assert_eq!(pending, !completed);
    }

    let invalid: i64 = database.connection.query_row(
        "SELECT count(*) FROM (SELECT blob_id FROM objects UNION ALL SELECT blob_id FROM multipart_parts) AS r
         LEFT JOIN blobs AS b ON b.blob_id = r.blob_id WHERE b.state IS NULL OR b.state <> 'ready'
         OR b.byte_length <> 3 OR b.md5_digest <> x'900150983cd24fb0d6963f7d28e17f72'",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(invalid, 0);
    Ok(())
}

fn history(actions: &[Action], log: &mut std::fs::File) -> TestResult {
    assert!(actions.len() <= 8);
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("metadata.sqlite3");
    let mut database = initialized(&directory)?;
    let (request, facts) = fixture(&mut database)?;
    let mut selected = database.select_completion(&Authority::Native, &request)?;
    let mut model = Model::default();
    let mut generations = vec![vec![17; 16]];
    assert_state(&database, &model, &generations)?;

    for (step, action) in actions.iter().copied().enumerate() {
        writeln!(
            log,
            "{}",
            serde_json::json!({"step": step, "action": format!("{action:?}"), "before": format!("{model:?}")})
        )?;

        log.flush()?;
        let expected = model.apply(action);

        let actual = if action == Action::Reopen {
            close(database)?;
            database = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
            Outcome::Success
        } else {
            observed(
                invoke(
                    &mut database,
                    &request,
                    &mut selected,
                    &facts,
                    action,
                    u8::try_from(step)?,
                ),
                action,
            )
        };

        writeln!(
            log,
            "{}",
            serde_json::json!({"step": step, "expected": format!("{expected:?}"), "actual": format!("{actual:?}")})
        )?;

        log.flush()?;
        assert_eq!(actual, expected, "{actions:?}, step {step}");

        if action == Action::Replace && actual == Outcome::Success {
            let generation: Vec<u8> = database.connection.query_row(
                "SELECT generation FROM multipart_parts",
                [],
                |row| row.get(0),
            )?;

            assert_eq!(generation.len(), 16);
            assert!(!generations.contains(&generation));
            generations.push(generation);
        }

        assert_state(&database, &model, &generations)?;
    }

    close(database)?;

    for _ in 0..2 {
        let reopened = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
        assert_state(&reopened, &model, &generations)?;
        close(reopened)?;
    }

    directory.finish()?;
    Ok(())
}

#[allow(
    clippy::print_stdout,
    reason = "qualification retains generated history observations"
)]
fn recorded(histories: impl IntoIterator<Item = Vec<Action>>) -> TestResult {
    let artifacts =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/model-histories");

    std::fs::create_dir_all(&artifacts)?;
    let directory = TestDirectory::new_in(&artifacts)?.retain_on_failure();
    let mut log = std::fs::File::create_new(directory.path().join("multipart.jsonl"))?;

    for (index, actions) in histories.into_iter().enumerate() {
        writeln!(
            log,
            "{}",
            serde_json::json!({"history": index, "actions": format!("{actions:?}")})
        )?;

        history(&actions, &mut log)?;
    }

    log.flush()?;
    drop(log);
    println!("MODEL_HISTORY_ARTIFACT {}", directory.preserve().display());
    Ok(())
}

#[test]
fn all_short_upload_orders_match_the_independent_claim_and_generation_model() -> TestResult {
    recorded(
        ACTIONS
            .into_iter()
            .flat_map(|first| ACTIONS.map(|second| vec![first, second])),
    )
}

#[test]
fn abort_recovery_and_reopen_histories_preserve_only_the_current_completion_owner() -> TestResult {
    recorded([
        vec![Action::Claim(60), Action::Abort, Action::Complete(60)],
        vec![Action::Claim(60), Action::Recover(60), Action::Complete(60)],
        vec![
            Action::Claim(60),
            Action::Recover(60),
            Action::Claim(62),
            Action::Complete(60),
            Action::Complete(62),
        ],
        vec![
            Action::Select,
            Action::Replace,
            Action::Claim(60),
            Action::Select,
            Action::Claim(62),
            Action::Reopen,
            Action::Complete(62),
        ],
        vec![
            Action::Claim(60),
            Action::Reopen,
            Action::Complete(60),
            Action::Abort,
            Action::Recover(60),
        ],
    ])
}
