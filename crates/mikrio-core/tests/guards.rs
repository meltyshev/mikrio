//! Native process and internal initializer probes, not public Store/power-loss evidence.

#[cfg(unix)]
#[path = "support/inheritance.rs"]
mod inheritance;
#[cfg(unix)]
#[path = "support/resources.rs"]
mod resources;
mod support;

use mikrio_core::{GuardManager, GuardMode, GuardTarget, OpenedStore};
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};

#[cfg(unix)]
use std::os::unix::fs::{symlink as symlink_dir, symlink as symlink_file};
#[cfg(windows)]
use std::os::windows::fs::{symlink_dir, symlink_file};

use support::{Probe, TestDirectory};

#[cfg(any(unix, windows))]
#[test]
fn a_symlinked_shard_directory_cannot_redirect_guard_acquisition() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;

    let store = manager
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    let shard = manager
        .try_acquire(GuardTarget::Shard(1), GuardMode::Shared)?
        .unwrap();

    let locks = directory.path().join("locks");
    let moved = directory.path().join("moved-locks");

    #[cfg(unix)]
    std::fs::rename(&locks, &moved)?;

    #[cfg(windows)]
    let shard = match std::fs::rename(&locks, &moved) {
        Ok(()) => Some(shard),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            // NTFS can refuse moving a directory with a locked descendant.
            // Verify protection, then exercise reparse refusal after release.
            assert!(
                manager
                    .try_acquire(GuardTarget::Shard(1), GuardMode::Exclusive)?
                    .is_none()
            );

            drop(shard);
            std::fs::rename(&locks, &moved)?;
            None
        }
        Err(error) => return Err(error),
    };

    symlink_dir("moved-locks", directory.path().join("locks"))?;

    let error = manager
        .try_acquire(GuardTarget::Shard(1), GuardMode::Shared)
        .err()
        .unwrap();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    drop(shard);
    drop(store);
    directory.finish()
}

#[test]
fn replacing_a_held_guard_never_admits_a_second_identity() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;

    let holder = manager
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    let path = directory.path().join(".mikrio.lock");
    std::fs::rename(&path, directory.path().join("held.lock"))?;
    std::fs::File::create_new(&path)?;

    for mode in [GuardMode::Shared, GuardMode::Exclusive] {
        let error = manager.try_acquire(GuardTarget::Store, mode).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("held.lock"))?;

    assert!(matches!(
        held.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));

    drop(held);
    drop(holder);
    directory.finish()
}

#[cfg(any(unix, windows))]
#[test]
fn guard_symlinks_are_refused_without_locking_their_target() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let path = directory.path().join(".mikrio.lock");
    std::fs::rename(&path, directory.path().join("sentinel"))?;
    symlink_file("sentinel", &path)?;
    let manager = GuardManager::open(directory.path())?;

    assert_eq!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Shared)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );

    let sentinel = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("sentinel"))?;

    sentinel.try_lock().map_err(io::Error::other)?;
    drop(sentinel);
    directory.finish()
}

#[test]
fn closing_one_shared_holder_preserves_the_other() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let first = GuardManager::open(directory.path())?;
    let second = GuardManager::open(directory.path())?;

    let first_guard = first
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    let second_guard = second
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    drop(first_guard);
    drop(first);

    assert!(
        second
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    let mut blocked = Probe::start(directory.path(), "exclusive", "guard_child")?;
    assert_eq!(blocked.observation()?, "BUSY");
    blocked.finish()?;

    drop(second_guard);
    let mut admitted = Probe::start(directory.path(), "exclusive", "guard_child")?;
    assert_eq!(admitted.observation()?, "LOCKED");
    admitted.finish()?;
    directory.finish()
}

#[test]
fn process_death_releases_its_guard() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;
    let mut holder = Probe::start(directory.path(), "shared", "guard_child")?;
    assert_eq!(holder.observation()?, "LOCKED");

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    holder.kill_and_wait()?;

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_some()
    );

    directory.finish()
}

#[test]
fn spawned_process_does_not_inherit_guard_ownership() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;

    let holder = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    let mut child = Probe::start(directory.path(), "exclusive", "guard_child")?;
    assert_eq!(child.observation()?, "BUSY");

    drop(holder);

    // The child is still alive, but must not retain the parent's file descriptor.
    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_some()
    );

    child.finish()?;
    directory.finish()
}

#[test]
fn shared_descriptor_duplicate_closure_preserves_the_original_lock() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join(".mikrio.lock"))?;

    file.lock_shared()?;
    let duplicate = file.try_clone()?;
    drop(duplicate);

    let mut contender = Probe::start(directory.path(), "exclusive", "guard_child")?;
    assert_eq!(contender.observation()?, "BUSY");
    contender.finish()?;
    drop(file);
    directory.finish()
}

#[test]
fn partial_multishard_acquisition_releases_only_its_own_guards() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;

    let store = manager
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    let held = manager
        .try_acquire(GuardTarget::Shard(2), GuardMode::Exclusive)?
        .unwrap();

    assert!(
        manager
            .try_acquire_shards(&[2, 1, 1], GuardMode::Shared)?
            .is_none()
    );

    assert!(
        manager
            .try_acquire(GuardTarget::Shard(1), GuardMode::Exclusive)?
            .is_some()
    );

    assert!(
        manager
            .try_acquire(GuardTarget::Shard(2), GuardMode::Shared)?
            .is_none()
    );

    assert!(
        manager
            .try_acquire(GuardTarget::Shard(3), GuardMode::Exclusive)?
            .is_some()
    );

    drop(held);

    let unique = manager
        .try_acquire_shards(&[2, 1, 2, 1], GuardMode::Exclusive)?
        .unwrap();

    assert!(
        manager
            .try_acquire(GuardTarget::Shard(1), GuardMode::Shared)?
            .is_none()
    );

    drop(unique);
    drop(store);
    directory.finish()
}

#[test]
fn missing_guard_is_an_error_and_is_never_recreated() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;
    let target = directory.path().join("locks/04.lock");
    assert!(!target.exists());

    let result = manager.try_acquire(GuardTarget::Shard(4), GuardMode::Shared);
    assert!(matches!(result, Err(ref error) if error.kind() == io::ErrorKind::NotFound));
    assert!(!target.exists());
    directory.finish()
}

#[cfg(any(unix, windows))]
#[test]
fn aliases_share_accounting_and_restored_directories_remain_independent() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let restored = TestDirectory::new()?;
    let alias = directory.path().join("alias");
    symlink_dir(directory.path(), &alias)?;
    let manager = GuardManager::open(directory.path())?;
    let alias_manager = GuardManager::open(&alias)?;
    let restored_manager = GuardManager::open(restored.path())?;

    let holder = manager
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    let alias_holder = alias_manager
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    drop(holder);

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    assert!(
        restored_manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_some()
    );

    drop(alias_holder);
    restored.finish()?;
    directory.finish()
}

#[test]
fn native_worker_retains_guard_after_caller_drops_its_handle() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    std::thread::scope(|scope| -> io::Result<()> {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);

        let worker = scope.spawn(move || {
            let _guard = guard;
            entered_tx.send(()).unwrap();
            // Unwinding drops the sender, then the scope joins this worker.
            let _released = release_rx.recv();
        });

        entered_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();

        drop(manager);
        let contender = GuardManager::open(directory.path())?;

        assert!(
            contender
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_none()
        );

        release_tx.send(()).unwrap();
        worker.join().unwrap();

        assert!(
            contender
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_some()
        );

        Ok(())
    })?;

    directory.finish()
}

#[test]
fn initialized_store_survives_independent_close_and_process_death() -> io::Result<()> {
    let _scenario = support::process_scenario()?;
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let store = OpenedStore::open(&path, Some("eu-central-1")).map_err(io::Error::other)?;
    let record = store.record().clone();
    let mut child = Probe::start(&path, "store", "guard_child")?;

    assert_eq!(
        child.observation()?,
        format!("STORE {} eu-central-1", record.store_id)
    );

    store
        .close()
        .map_err(|(_, error)| io::Error::other(error))?;

    let mut contender = Probe::start(&path, "exclusive", "guard_child")?;
    assert_eq!(contender.observation()?, "BUSY");
    contender.finish()?;
    child.kill_and_wait()?;

    let manager = GuardManager::open(&path)?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    drop(guard);
    let reopened = OpenedStore::open(&path, None).map_err(io::Error::other)?;
    assert_eq!(reopened.record(), &record);

    reopened
        .close()
        .map_err(|(_, error)| io::Error::other(error))?;

    directory.finish()
}

#[test]
#[ignore = "owned subprocess entry; invoked by the process probes, not a standalone scenario"]
#[allow(
    clippy::print_stdout,
    reason = "test-only subprocess handshake over an owned pipe"
)]
fn guard_child() -> io::Result<()> {
    let path = std::env::var_os("mikrio_probe_directory")
        .ok_or_else(|| io::Error::other("missing owned test directory"))?;

    if std::env::var("mikrio_probe_mode").as_deref() == Ok("barrier") {
        println!("PROBE EXEC");
        io::stdout().flush()?;
        return wait_for_shutdown();
    }

    if std::env::var("mikrio_probe_mode").as_deref() == Ok("store") {
        let store =
            OpenedStore::open(std::path::Path::new(&path), None).map_err(io::Error::other)?;

        println!(
            "PROBE STORE {} {}",
            store.record().store_id,
            store.record().region
        );

        io::stdout().flush()?;
        wait_for_shutdown()?;
        return store.close().map_err(|(_, error)| io::Error::other(error));
    }

    let mode = match std::env::var("mikrio_probe_mode").as_deref() {
        Ok("shared") => GuardMode::Shared,
        Ok("exclusive") => GuardMode::Exclusive,
        _ => return Err(io::Error::other("invalid probe mode")),
    };

    let manager = GuardManager::open(std::path::Path::new(&path))?;
    let guard = manager.try_acquire(GuardTarget::Store, mode)?;
    println!("PROBE {}", if guard.is_some() { "LOCKED" } else { "BUSY" });
    io::stdout().flush()?;

    wait_for_shutdown()?;
    drop(guard);
    Ok(())
}

fn wait_for_shutdown() -> io::Result<()> {
    let mut command = String::new();
    io::stdin().lock().read_line(&mut command)?;

    if command != "exit\n" {
        return Err(io::Error::other("parent did not request normal shutdown"));
    }

    Ok(())
}
