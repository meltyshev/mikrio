//! Competing transactions must match one complete order in the independent model.

use super::*;
use std::sync::Barrier;

fn allowed(actions: [Action; 2], outcomes: [Outcome; 2]) -> Vec<Model> {
    let mut candidates = Vec::new();

    for order in [[0, 1], [1, 0]] {
        let mut model = Model::default();

        if !order
            .into_iter()
            .all(|index| model.apply(actions[index]) == outcomes[index])
        {
            continue;
        }

        if !candidates.contains(&model) {
            candidates.push(model);
        }
    }

    candidates
}

#[test]
fn checker_rejects_expiring_a_concurrently_replaced_or_retagged_generation() {
    for action in [Action::Replace, Action::Retag] {
        let candidates = allowed([action, Action::Expire], [Outcome::Success; 2]);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].live);
    }

    let expired_first = allowed(
        [Action::Retag, Action::Expire],
        [Outcome::NoSuchKey, Outcome::Success],
    );

    assert_eq!(expired_first.len(), 1);
    assert!(!expired_first[0].live);

    assert_eq!(
        allowed(
            [Action::Configure(true), Action::Expire],
            [Outcome::NoSuchKey, Outcome::Success]
        ),
        Vec::<Model>::new()
    );
}

fn race(action: Action, log: &mut std::fs::File) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("metadata.sqlite3");
    let (mut database, request) = setup(&directory)?;
    publish(&mut database, &request, 0)?;
    database.put_bucket_lifecycle(&Authority::Native, &bucket(&request), &rules("", 1, true))?;
    let selected = candidate(&mut database, DUE)?;
    let mut known = Identities::default();
    assert_state(&mut database, &request, Model::default(), &mut known)?;

    let peers = [
        MetadataDatabase::open(&path, MetadataOpen::Existing)?,
        MetadataDatabase::open(&path, MetadataOpen::Existing)?,
    ];

    let actions = [action, Action::Expire];
    let barrier = Barrier::new(3);

    writeln!(
        log,
        "{}",
        serde_json::json!({"invoked": format!("{actions:?}")})
    )?;

    log.flush()?;

    let outcomes = std::thread::scope(|scope| {
        let workers: Vec<_> = peers
            .into_iter()
            .zip(actions)
            .map(|(mut peer, action)| {
                let request = &request;
                let barrier = &barrier;
                let mut selected = selected.clone();

                scope.spawn(move || {
                    barrier.wait();
                    // Errors panic inside this worker so the originating
                    // non-Send error need not be transported across threads.
                    let result = invoke(&mut peer, request, &mut selected, action, true).unwrap();
                    (peer, result)
                })
            })
            .collect();

        barrier.wait();

        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });

    let [first, second]: [_; 2] = outcomes.try_into().ok().unwrap();
    let outcomes = [first.1, second.1];
    close(first.0)?;
    close(second.0)?;
    let candidates = allowed(actions, outcomes);
    let live = visible(&mut database, &request);

    let matching: Vec<_> = candidates
        .iter()
        .filter(|model| model.live == live)
        .collect();

    writeln!(
        log,
        "{}",
        serde_json::json!({"replied": format!("{outcomes:?}"), "live": live, "allowed": format!("{candidates:?}")})
    )?;

    log.flush()?;
    assert_eq!(matching.len(), 1, "{actions:?}: {outcomes:?}");
    let model = *matching[0];
    assert_state(&mut database, &request, model, &mut known)?;
    close(database)?;

    for _ in 0..2 {
        let mut reopened = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
        assert_state(&mut reopened, &request, model, &mut known)?;
        close(reopened)?;
    }

    directory.finish()?;
    Ok(())
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "qualification retains overlapping transaction observations"
)]
fn overlapping_replacement_retag_and_rule_transactions_match_expiration_model_orders() -> TestResult
{
    let artifacts =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/model-histories");

    std::fs::create_dir_all(&artifacts)?;
    let directory = TestDirectory::new_in(&artifacts)?.retain_on_failure();
    let mut log = std::fs::File::create_new(directory.path().join("lifecycle-concurrent.jsonl"))?;

    for round in 0..8 {
        for action in [
            Action::Replace,
            Action::Retag,
            Action::Configure(false),
            Action::Configure(true),
        ] {
            writeln!(log, "{}", serde_json::json!({"round": round}))?;
            race(action, &mut log)?;
        }
    }

    log.flush()?;
    drop(log);
    println!("MODEL_HISTORY_ARTIFACT {}", directory.preserve().display());
    Ok(())
}
