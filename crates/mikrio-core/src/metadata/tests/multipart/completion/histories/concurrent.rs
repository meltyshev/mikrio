//! Both invocations overlap; either whole transaction order is permitted.

use super::*;
use std::sync::Barrier;

fn allowed(initial: &Model, actions: [Action; 2], outcomes: [Outcome; 2]) -> Vec<Model> {
    let mut candidates = Vec::new();

    for order in [[0, 1], [1, 0]] {
        let mut model = initial.clone();

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
fn checker_rejects_two_completion_winners_and_an_unfenced_part_replacement() {
    for actions in [
        [Action::Claim(60), Action::Claim(62)],
        [Action::Replace, Action::Claim(60)],
    ] {
        assert_eq!(
            allowed(&Model::default(), actions, [Outcome::Success; 2]),
            Vec::<Model>::new()
        );
    }

    let mut claimed = Model::default();
    claimed.apply(Action::Claim(60));

    assert_eq!(
        allowed(
            &claimed,
            [Action::Recover(60), Action::Complete(60)],
            [Outcome::Collected(true), Outcome::Success]
        ),
        Vec::<Model>::new()
    );
}

fn race(actions: [Action; 2], claimed: bool, log: &mut std::fs::File) -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("metadata.sqlite3");
    let mut database = initialized(&directory)?;
    let (request, facts) = fixture(&mut database)?;
    let mut selected = database.select_completion(&Authority::Native, &request)?;

    let selections = [
        database.select_completion(&Authority::Native, &request)?,
        database.select_completion(&Authority::Native, &request)?,
    ];

    assert!(selections.iter().all(|selection| selection == &selected));
    let mut model = Model::default();
    let mut generations = vec![vec![17; 16]];

    if claimed {
        assert_eq!(
            invoke(
                &mut database,
                &request,
                &mut selected,
                &facts,
                Action::Claim(60),
                0,
            )?,
            model.apply(Action::Claim(60))
        );
    }

    assert_state(&database, &model, &generations)?;

    let peers = [
        MetadataDatabase::open(&path, MetadataOpen::Existing)?,
        MetadataDatabase::open(&path, MetadataOpen::Existing)?,
    ];

    let barrier = Barrier::new(3);

    writeln!(
        log,
        "{}",
        serde_json::json!({"invoked": format!("{actions:?}"), "claimed": claimed})
    )?;

    log.flush()?;

    let outcomes = std::thread::scope(|scope| {
        let workers: Vec<_> = peers
            .into_iter()
            .zip(actions)
            .zip(selections)
            .enumerate()
            .map(|(index, ((mut peer, action), mut selected))| {
                let request = &request;
                let facts = &facts;
                let barrier = &barrier;

                scope.spawn(move || {
                    barrier.wait();

                    let result = invoke(
                        &mut peer,
                        request,
                        &mut selected,
                        facts,
                        action,
                        u8::try_from(index).unwrap(),
                    );

                    (peer, observed(result, action))
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
    let candidates = allowed(&model, actions, outcomes);

    writeln!(
        log,
        "{}",
        serde_json::json!({"replied": format!("{outcomes:?}"), "allowed": format!("{candidates:?}")})
    )?;

    log.flush()?;
    assert_eq!(candidates.len(), 1, "{actions:?}: {outcomes:?}");
    let model = &candidates[0];

    if actions
        .into_iter()
        .zip(outcomes)
        .any(|(action, outcome)| action == Action::Replace && outcome == Outcome::Success)
    {
        let generation: Vec<u8> =
            database
                .connection
                .query_row("SELECT generation FROM multipart_parts", [], |row| {
                    row.get(0)
                })?;

        assert_eq!(generation.len(), 16);
        assert!(!generations.contains(&generation));
        generations.push(generation);
    }

    assert_state(&database, model, &generations)?;
    close(database)?;

    for _ in 0..2 {
        let reopened = MetadataDatabase::open(&path, MetadataOpen::Existing)?;
        assert_state(&reopened, model, &generations)?;
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
fn overlapping_part_claim_abort_and_recovery_transactions_match_a_whole_model_order() -> TestResult
{
    let artifacts =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/model-histories");

    std::fs::create_dir_all(&artifacts)?;
    let directory = TestDirectory::new_in(&artifacts)?.retain_on_failure();
    let mut log = std::fs::File::create_new(directory.path().join("multipart-concurrent.jsonl"))?;

    for round in 0..8 {
        for (actions, claimed) in [
            ([Action::Replace, Action::Claim(60)], false),
            ([Action::Claim(60), Action::Claim(62)], false),
            ([Action::Claim(60), Action::Abort], false),
            ([Action::Recover(60), Action::Complete(60)], true),
        ] {
            writeln!(log, "{}", serde_json::json!({"round": round}))?;
            race(actions, claimed, &mut log)?;
        }
    }

    log.flush()?;
    drop(log);
    println!("MODEL_HISTORY_ARTIFACT {}", directory.preserve().display());
    Ok(())
}
