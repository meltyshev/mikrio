//! A selected hint is tested against an independent identity/rule/clock model.
//! These histories exercise real metadata transactions, not payload durability.

use std::io::Write as _;

use super::*;

mod concurrent;
mod model;
use model::{Action, Model, Outcome};

const ACTIONS: [Action; 10] = [
    Action::Replace,
    Action::Retag,
    Action::Configure(false),
    Action::Configure(true),
    Action::Recreate,
    Action::Select,
    Action::Expire,
    Action::Clock(false),
    Action::Clock(true),
    Action::Reopen,
];

#[derive(Default)]
struct Identities {
    buckets: Vec<Vec<u8>>,
    objects: Vec<Vec<u8>>,
    rules: Vec<Vec<u8>>,
}

fn identity(known: &mut Vec<Vec<u8>>, generation: u8, actual: &[u8]) {
    assert_eq!(actual.len(), 16);
    let index = usize::from(generation);

    if index == known.len() {
        assert!(!known.iter().any(|value| value == actual));
        known.push(actual.to_vec());
    }

    assert_eq!(known[index], actual);
}

fn assert_state(
    database: &mut MetadataDatabase,
    request: &PutRequest,
    model: Model,
    known: &mut Identities,
) -> TestResult {
    let (bucket_id, rule_generation): (Vec<u8>, Vec<u8>) = database.connection.query_row(
        "SELECT bucket_id, lifecycle_generation FROM buckets WHERE name = ?1",
        [request.bucket.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    identity(&mut known.buckets, model.bucket, &bucket_id);
    identity(&mut known.rules, model.rules, &rule_generation);

    let selected = match database.select_object(&Authority::Native, &read(request)) {
        Ok(object) => Some(object),
        Err(error) if error.failure() == OperationFailure::NoSuchKey => None,
        Err(error) => return Err(error.into()),
    };

    assert_eq!(selected.is_some(), model.live);

    if let Some(object) = selected {
        assert_eq!(object.bucket_id.as_slice(), bucket_id);
        assert_eq!(object.length.get(), 15);
        assert_eq!(object.etag, "\"d862b34b7565f1664601ba3db1554ffc\"");
        identity(&mut known.objects, model.object, &object.generation);
    }

    assert_eq!(
        database.get_bucket_lifecycle(&Authority::Native, &bucket(request))?,
        Some(rules("", 1, model.enabled))
    );

    let invalid: i64 = database.connection.query_row(
        "SELECT count(*) FROM blobs AS b LEFT JOIN objects AS o ON o.blob_id = b.blob_id
         WHERE (o.blob_id IS NULL AND b.gc_pending <> 1)
         OR (o.blob_id IS NOT NULL AND (b.state <> 'ready' OR b.gc_pending <> 0
             OR b.byte_length <> 15 OR b.md5_digest <> x'd862b34b7565f1664601ba3db1554ffc'))",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(invalid, 0);
    Ok(())
}

#[allow(
    clippy::unreachable,
    reason = "the driver replaces the connection for reopen"
)]
fn invoke(
    database: &mut MetadataDatabase,
    request: &PutRequest,
    selected: &mut Candidate,
    action: Action,
    due: bool,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let now = if due { DUE } else { DUE - 1 };

    match action {
        Action::Replace => {
            publish(database, request, 0)?;
        }
        Action::Retag => {
            let result = database.replace_object_tags(
                &read(request),
                &crate::object::ObjectTags::new([("state".into(), "attached".into())])?,
            );

            if let Err(error) = result {
                assert_eq!(error.failure(), OperationFailure::NoSuchKey);
                assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
                return Ok(Outcome::NoSuchKey);
            }
        }
        Action::Configure(enabled) => {
            database.put_bucket_lifecycle(
                &Authority::Native,
                &bucket(request),
                &rules("", 1, enabled),
            )?;
        }
        Action::Recreate => {
            database.delete_object(&Authority::Native, &read(request), None)?;
            database.delete_bucket(&request.bucket, None)?;
            database.create_bucket(&request.bucket)?;
            publish(database, request, 0)?;

            database.put_bucket_lifecycle(
                &Authority::Native,
                &bucket(request),
                &rules("", 1, true),
            )?;
        }
        Action::Select => {
            let mut found = false;

            if let Some(page) = database.object_expiration_page(clock(now))? {
                for (_, value) in page.candidates {
                    if let Some(candidate) = value? {
                        assert!(!found);
                        *selected = candidate;
                        found = true;
                    }
                }
            }

            return Ok(Outcome::Selected(found));
        }
        Action::Expire => database.expire_object(selected, clock(now))?,
        Action::Clock(_) => {}
        Action::Reopen => unreachable!("connection replacement belongs to the driver"),
    }

    Ok(Outcome::Success)
}

fn history(actions: &[Action], log: &mut std::fs::File) -> TestResult {
    assert!(actions.len() <= 8);
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("metadata.sqlite3");
    let (mut database, request) = setup(&directory)?;
    publish(&mut database, &request, 0)?;
    database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &rules("", 1, true))?;
    let mut selected = candidate(&mut database, DUE)?;
    let mut model = Model::default();
    let mut known = Identities::default();
    assert_state(&mut database, &request, model, &mut known)?;

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
            invoke(&mut database, &request, &mut selected, action, model.due)?
        };

        writeln!(
            log,
            "{}",
            serde_json::json!({"step": step, "expected": format!("{expected:?}"), "actual": format!("{actual:?}")})
        )?;

        log.flush()?;
        assert_eq!(actual, expected, "{actions:?}, step {step}");
        assert_state(&mut database, &request, model, &mut known)?;
    }

    close(database)?;

    for _ in 0..2 {
        let mut reopened = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
        assert_state(&mut reopened, &request, model, &mut known)?;
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
    let mut log = std::fs::File::create_new(directory.path().join("lifecycle.jsonl"))?;

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
fn all_short_expiration_orders_match_namespace_object_rule_and_clock_generations() -> TestResult {
    recorded(
        ACTIONS
            .into_iter()
            .flat_map(|first| ACTIONS.map(|second| vec![first, second])),
    )
}

#[test]
fn stale_expiration_retries_and_recreated_names_match_the_independent_model() -> TestResult {
    recorded([
        vec![
            Action::Select,
            Action::Replace,
            Action::Reopen,
            Action::Expire,
            Action::Select,
            Action::Expire,
        ],
        vec![
            Action::Configure(false),
            Action::Clock(false),
            Action::Reopen,
            Action::Configure(true),
            Action::Clock(true),
            Action::Expire,
            Action::Select,
            Action::Expire,
        ],
        vec![
            Action::Recreate,
            Action::Expire,
            Action::Retag,
            Action::Select,
            Action::Reopen,
            Action::Expire,
        ],
        vec![
            Action::Expire,
            Action::Retag,
            Action::Replace,
            Action::Expire,
            Action::Select,
            Action::Expire,
        ],
    ])
}
