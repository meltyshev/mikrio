//! Native process limits apply only to the owned child, never the test runner.

use std::io::{self, BufRead as _, Write as _};

use mikrio_core::{DurableDirectory, GuardManager, GuardMode, GuardTarget};

use super::support::{Probe, TestDirectory, process_scenario};

#[test]
fn native_short_writes_and_file_limit_errors_preserve_previous_files_and_guards() -> io::Result<()>
{
    let _scenario = process_scenario()?;
    let directory = TestDirectory::new()?;
    let staging_path = directory.path().join("staging");
    let blobs_path = directory.path().join("blobs");
    std::fs::create_dir(&staging_path)?;
    std::fs::create_dir(&blobs_path)?;
    let staging = DurableDirectory::open(&staging_path)?;
    let blobs = DurableDirectory::open(&blobs_path)?;

    blobs
        .create_file("previous")?
        .write_all(b"previous payload")?;

    let manager = GuardManager::open(directory.path())?;
    let mut child = Probe::start(directory.path(), "file-size", "resources::resource_child")?;
    assert_eq!(child.observation()?, "SHORT 7 FILE_TOO_LARGE");
    assert_eq!(std::fs::read(staging_path.join("single"))?, b"new pay");
    assert_eq!(std::fs::read(staging_path.join("all"))?, b"new pay");

    assert_eq!(
        std::fs::read(blobs_path.join("previous"))?,
        b"previous payload"
    );

    assert!(!blobs_path.join("new").try_exists()?);

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    child.finish()?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .unwrap();

    staging.remove_owned("single")?;
    staging.remove_owned("all")?;
    staging.create_file("retry")?.write_all(b"new payload")?;
    staging.publish_new("retry", &blobs, "new")?;
    assert_eq!(std::fs::read(blobs_path.join("new"))?, b"new payload");

    assert_eq!(
        std::fs::read(blobs_path.join("previous"))?,
        b"previous payload"
    );

    drop(guard);
    directory.finish()
}

#[test]
#[ignore = "owned resource subprocess entry; invoked by its parent scenario"]
#[allow(clippy::print_stdout, reason = "owned subprocess handshake")]
fn resource_child() -> io::Result<()> {
    let directory = std::path::PathBuf::from(
        std::env::var_os("mikrio_probe_directory").ok_or(io::ErrorKind::InvalidInput)?,
    );

    assert_eq!(std::env::var("mikrio_probe_mode").unwrap(), "file-size");
    let manager = GuardManager::open(&directory)?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    let staging = DurableDirectory::open(&directory.join("staging"))?;
    let mut single = staging.create_file("single")?;
    let mut all = staging.create_file("all")?;
    limit_file_size()?;

    assert_eq!(single.write(b"new payload")?, 7);
    let error = single.write(b"load").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EFBIG));
    let error = all.write_all(b"new payload").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EFBIG));
    assert_eq!(single.metadata()?.len(), 7);
    assert_eq!(all.metadata()?.len(), 7);
    println!("PROBE SHORT 7 FILE_TOO_LARGE");
    io::stdout().flush()?;

    let mut command = String::new();
    io::stdin().lock().read_line(&mut command)?;
    assert_eq!(command, "exit\n");
    drop(single);
    drop(all);
    drop(guard);
    Ok(())
}

#[allow(
    unsafe_code,
    reason = "test-only native process resource configuration"
)]
fn limit_file_size() -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: 7,
        rlim_max: 7,
    };

    // SAFETY: this isolated child owns its signal disposition and resource
    // limits. The OS reads an initialized native rlimit for the duration of the
    // call. SIG_IGN is a platform-defined disposition, not a Rust callback.
    unsafe {
        if libc::signal(libc::SIGXFSZ, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }

        if libc::setrlimit(libc::RLIMIT_FSIZE, &raw const limit) != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires an isolated finite tmpfs fixture"]
fn bounded_volume_exhaustion_preserves_previous_files_and_allows_cleanup() -> io::Result<()> {
    let volume =
        std::env::var_os("mikrio_bounded_test_volume").ok_or(io::ErrorKind::InvalidInput)?;

    let directory = TestDirectory::new_in(std::path::Path::new(&volume))?;
    let files = DurableDirectory::open(directory.path())?;

    files
        .create_file("previous")?
        .write_all(b"previous payload")?;

    let manager = GuardManager::open(directory.path())?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    let mut staging = files.create_file("partial")?;
    let chunk = vec![0xa5; 65_536];
    let mut written = 0;

    let failure = loop {
        assert!(
            written < 16 * 1024 * 1024,
            "volume did not enforce its finite budget"
        );

        match staging.write(&chunk) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(error) => break error,
        }
    };

    assert_eq!(failure.raw_os_error(), Some(libc::ENOSPC));
    assert!(written > 0);
    assert_eq!(staging.metadata()?.len(), u64::try_from(written).unwrap());
    let partial = std::fs::read(directory.path().join("partial"))?;
    assert_eq!(partial.len(), written);
    assert!(partial.iter().all(|byte| *byte == 0xa5));

    assert_eq!(
        std::fs::read(directory.path().join("previous"))?,
        b"previous payload"
    );

    assert!(!directory.path().join("new").try_exists()?);

    assert!(
        manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .is_none()
    );

    drop(staging);
    files.remove_owned("partial")?;
    files.create_file("retry")?.write_all(b"complete retry")?;
    files.publish_new("retry", &files, "new")?;

    assert_eq!(
        std::fs::read(directory.path().join("new"))?,
        b"complete retry"
    );

    assert_eq!(
        std::fs::read(directory.path().join("previous"))?,
        b"previous payload"
    );

    drop(guard);
    directory.finish()
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires an isolated finite tmpfs fixture"]
fn bounded_inode_exhaustion_keeps_owned_unlink_and_retry_available() -> io::Result<()> {
    let volume =
        std::env::var_os("mikrio_bounded_test_volume").ok_or(io::ErrorKind::InvalidInput)?;

    let directory = TestDirectory::new_in(std::path::Path::new(&volume))?;
    let files = DurableDirectory::open(directory.path())?;

    files
        .create_file("previous")?
        .write_all(b"previous payload")?;

    let mut created = Vec::new();

    let failure = loop {
        assert!(
            created.len() < 1024,
            "volume did not enforce its inode budget"
        );

        let name = format!("empty-{}", created.len());

        match files.create_file(&name) {
            Ok(file) => {
                assert_eq!(file.metadata()?.len(), 0);
                drop(file);
                created.push(name);
            }
            Err(error) => break error,
        }
    };

    assert_eq!(failure.raw_os_error(), Some(libc::ENOSPC));
    assert_ne!(created, Vec::<String>::new());

    assert_eq!(
        std::fs::read(directory.path().join("previous"))?,
        b"previous payload"
    );

    for name in created {
        files.remove_owned(&name)?;
    }

    files.create_file("retry")?.write_all(b"complete retry")?;
    files.publish_new("retry", &files, "new")?;

    assert_eq!(
        std::fs::read(directory.path().join("new"))?,
        b"complete retry"
    );

    assert_eq!(
        std::fs::read(directory.path().join("previous"))?,
        b"previous payload"
    );

    directory.finish()
}
