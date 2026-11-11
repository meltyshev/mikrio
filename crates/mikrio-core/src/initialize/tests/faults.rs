//! Kill real initializer effects. These observations cover process interruption,
//! not the ordering of writes that survive storage power loss.

use std::cell::Cell;
use std::io::BufRead as _;

use super::*;
use crate::test_process as process;

mod effects;

const CHILD: &str = "initialize::tests::faults::initializer_child";
const LAYOUT_PROGRESS: [&str; 7] = [
    "bootstrap.layout.parent_sync.after",
    "bootstrap.layout.parent_sync.after",
    "bootstrap.layout.guard_sync.after:64",
    "bootstrap.layout.guard_sync.after:128",
    "bootstrap.layout.guard_sync.after:192",
    "bootstrap.layout.guard_sync.after:256",
    "bootstrap.layout.locks_sync.after",
];

thread_local! {
    static SYNCED_GUARDS: Cell<u16> = const { Cell::new(0) };
}

fn guard_progress(completed: &Cell<u16>) -> io::Result<Option<&'static str>> {
    if completed.get() >= 256 {
        return Err(io::Error::other("unexpected additional guard sync"));
    }

    let count = completed.get() + 1;
    completed.set(count);

    Ok(match count {
        64 => Some(LAYOUT_PROGRESS[2]),
        128 => Some(LAYOUT_PROGRESS[3]),
        192 => Some(LAYOUT_PROGRESS[4]),
        256 => Some(LAYOUT_PROGRESS[5]),
        _ => None,
    })
}

fn checkpoint(point: &str) -> io::Result<()> {
    process::trace(point);
    let target = std::env::var("mikrio_probe_mode").map_err(io::Error::other)?;

    if target != point {
        let progress = match point {
            "bootstrap.layout.parent_sync.after" => Some(LAYOUT_PROGRESS[0]),
            "bootstrap.layout.guard_sync.after" => SYNCED_GUARDS.with(guard_progress)?,
            "bootstrap.layout.locks_sync.after" => Some(LAYOUT_PROGRESS[6]),
            _ => None,
        };

        if let Some(progress) = progress {
            // These messages follow completed durability effects, never a
            // timer. A stalled phase still exhausts its observation watchdog.
            let mut output = io::stdout().lock();
            writeln!(output, "PROBE INITIALIZER_PROGRESS {progress}")?;
            output.flush()?;
        }

        return Ok(());
    }

    let mut output = io::stdout().lock();
    writeln!(output, "PROBE INITIALIZER_CHECKPOINT")?;
    output.flush()?;
    drop(output);
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    if line != "exit\n" {
        return Err(io::Error::other("test control pipe closed"));
    }

    Ok(())
}

fn expected_progress(point: &str) -> &'static [&'static str] {
    let count = match point {
        "bootstrap.layout.guard_create.after" | "bootstrap.layout.guard_sync.after" => 2,
        "bootstrap.layout.locks_sync.after" => 6,
        point if point.starts_with("bootstrap.sql.") || point.starts_with("ready.") => 7,
        _ => 0,
    };

    &LAYOUT_PROGRESS[..count]
}

fn reached_checkpoint(
    observation: &str,
    expected: &[&str],
    completed: &mut usize,
) -> io::Result<bool> {
    if observation == "INITIALIZER_CHECKPOINT" && *completed == expected.len() {
        return Ok(true);
    }

    if let Some(point) = expected.get(*completed)
        && observation == format!("INITIALIZER_PROGRESS {point}")
    {
        *completed += 1;
        return Ok(false);
    }

    Err(io::Error::other("unexpected initializer phase observation"))
}

#[test]
fn initializer_progress_requires_completed_ordered_phases_before_the_checkpoint() -> TestResult {
    for point in [
        "initializing.marker.publish.before",
        "bootstrap.layout.guard_create.after",
        "bootstrap.layout.guard_sync.after",
        "bootstrap.layout.locks_sync.after",
        "bootstrap.sql.begin.before",
        "ready.marker.rename.after",
    ] {
        let expected = expected_progress(point);
        let mut completed = 0;

        for phase in expected {
            assert!(
                reached_checkpoint("INITIALIZER_CHECKPOINT", expected, &mut completed).is_err()
            );

            assert!(!reached_checkpoint(
                &format!("INITIALIZER_PROGRESS {phase}"),
                expected,
                &mut completed,
            )?);
        }

        assert!(reached_checkpoint(
            "INITIALIZER_CHECKPOINT",
            expected,
            &mut completed
        )?);

        assert!(
            reached_checkpoint(
                "INITIALIZER_PROGRESS bootstrap.layout.parent_sync.after",
                expected,
                &mut completed
            )
            .is_err()
        );
    }

    for unexpected in [
        "INITIALIZER_PROGRESS bootstrap.layout.locks_sync.after",
        "INITIALIZER_PROGRESS bootstrap.layout.shard_sync.after",
        "INITIALIZER_PROGRESS bootstrap.layout.guard_sync.after:64",
        "INITIALIZER_PROGRESS",
        "HEARTBEAT",
    ] {
        let mut completed = 0;
        assert!(reached_checkpoint(unexpected, &LAYOUT_PROGRESS, &mut completed).is_err());
        assert_eq!(completed, 0);
    }

    let guards = Cell::new(0);
    let mut completed = 2;

    for count in 1..=256 {
        let message = guard_progress(&guards)?;
        assert_eq!(guards.get(), count);

        if let Some(message) = message {
            assert_eq!(count % 64, 0);

            assert!(!reached_checkpoint(
                &format!("INITIALIZER_PROGRESS {message}"),
                &LAYOUT_PROGRESS,
                &mut completed,
            )?);

            let after_group = completed;

            assert!(
                reached_checkpoint(
                    &format!("INITIALIZER_PROGRESS {message}"),
                    &LAYOUT_PROGRESS,
                    &mut completed,
                )
                .is_err()
            );

            assert_eq!(completed, after_group);

            assert!(
                reached_checkpoint("INITIALIZER_CHECKPOINT", &LAYOUT_PROGRESS, &mut completed)
                    .is_err()
            );
        } else {
            assert_ne!(count % 64, 0);
        }
    }

    assert_eq!(completed, 6);
    assert!(guard_progress(&guards).is_err());
    assert_eq!(guards.get(), 256);

    for unexpected in [
        "INITIALIZER_PROGRESS bootstrap.layout.guard_sync.after:64",
        "INITIALIZER_PROGRESS bootstrap.layout.guard_sync.after:256",
        "INITIALIZER_PROGRESS bootstrap.layout.guard_sync.after:257",
    ] {
        assert!(reached_checkpoint(unexpected, &LAYOUT_PROGRESS, &mut completed).is_err());
        assert_eq!(completed, 6);
    }

    assert!(!reached_checkpoint(
        "INITIALIZER_PROGRESS bootstrap.layout.locks_sync.after",
        &LAYOUT_PROGRESS,
        &mut completed,
    )?);

    assert!(reached_checkpoint(
        "INITIALIZER_CHECKPOINT",
        &LAYOUT_PROGRESS,
        &mut completed,
    )?);

    Ok(())
}

fn kills(points: &[(&str, Option<OpenFailure>)]) -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for (index, (point, refusal)) in points.iter().enumerate() {
        std::fs::write(directory.path().join("active-case.txt"), point)?;
        let path = directory.path().join(format!("store-{index}"));
        let mut child = process::Probe::start(&path, point, CHILD)?;
        let mut completed = 0;

        while !reached_checkpoint(
            &child.observation()?,
            expected_progress(point),
            &mut completed,
        )? {}

        if path.join("store.json").try_exists()? {
            assert_eq!(failure(&path, None), OpenFailure::StoreBusy, "{point}");
        }

        child.kill_and_wait()?;

        if let Some(expected) = refusal {
            // These interruptions precede ownership proof or leave a malformed
            // marker temporary. Capture every file: refusal must preserve it.
            let mut files = fs::read_dir(&path)?
                .map(|entry| {
                    let entry = entry?;
                    Ok((entry.path(), entry.file_type()?))
                })
                .collect::<io::Result<Vec<_>>>()?;

            files.retain(|(_, kind)| kind.is_file());

            let before = files
                .iter()
                .map(|(path, _)| fs::read(path))
                .collect::<io::Result<Vec<_>>>()?;

            assert_eq!(failure(&path, None), *expected, "{point}");

            let after = files
                .iter()
                .map(|(path, _)| fs::read(path))
                .collect::<io::Result<Vec<_>>>()?;

            assert!(before == after, "refusal changed state at {point}");
            continue;
        }

        let recovered = OpenedStore::open(&path, Some("eu-west-1"))?;
        let record = recovered.record().clone();
        assert_eq!(record.region, "eu-west-1");
        close(recovered)?;
        let second = OpenedStore::open(&path, None)?;
        assert_eq!(second.record(), &record, "{point}");
        close(second)?;
    }

    directory.finish()?;
    Ok(())
}

fn fail_schema(point: &str) -> io::Result<()> {
    if point == "bootstrap.sql.schema.after" {
        return Err(io::Error::other("test schema acknowledgment failure"));
    }

    Ok(())
}

fn fail_commit(point: &str) -> io::Result<()> {
    if point == "bootstrap.sql.commit.after" {
        return Err(io::Error::other("test commit acknowledgment failure"));
    }

    Ok(())
}

#[test]
fn initializer_statement_and_commit_acknowledgment_errors_leave_recoverable_state() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let callbacks: [fn(&str) -> io::Result<()>; 2] = [fail_schema, fail_commit];

    for (index, callback) in callbacks.into_iter().enumerate() {
        let path = directory.path().join(format!("store-{index}"));

        {
            let _scope = fault::install(callback);
            assert_eq!(failure(&path, Some("eu-west-1")), OpenFailure::IoError);
        }

        let store = OpenedStore::open(&path, Some("eu-west-1"))?;
        assert_eq!(store.record().region, "eu-west-1");
        close(store)?;
    }

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry for initializer interruption scenarios"]
fn initializer_child() -> TestResult {
    let directory = std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?;
    SYNCED_GUARDS.set(0);
    let _fault = fault::install(checkpoint);
    close(OpenedStore::open(Path::new(&directory), Some("eu-west-1"))?)?;
    Ok(())
}
