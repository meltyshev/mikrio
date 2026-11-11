use super::*;
use std::io::Write;

use crate::test_directory as directory;

const PUBLICATION: &[(Effect, Phase)] = &[
    (Effect::FileSync, Phase::Before),
    (Effect::FileSync, Phase::After),
    #[cfg(target_os = "macos")]
    (Effect::DeviceSync, Phase::Before),
    #[cfg(target_os = "macos")]
    (Effect::DeviceSync, Phase::After),
    (Effect::Rename, Phase::Before),
    (Effect::Rename, Phase::After),
    (Effect::DestinationSync, Phase::Before),
    (Effect::DestinationSync, Phase::After),
    (Effect::SourceSync, Phase::Before),
    (Effect::SourceSync, Phase::After),
];

const REMOVAL: [(Effect, Phase); 4] = [
    (Effect::Unlink, Phase::Before),
    (Effect::Unlink, Phase::After),
    (Effect::UnlinkSync, Phase::Before),
    (Effect::UnlinkSync, Phase::After),
];

#[cfg(target_os = "macos")]
#[test]
fn native_permission_denials_preserve_owned_files_and_do_not_advance_effects() -> io::Result<()> {
    let temporary = directory::TestDirectory::new()?;
    let staging_path = temporary.path().join("staging");
    let blobs_path = temporary.path().join("blobs");
    std::fs::create_dir(&staging_path)?;
    std::fs::create_dir(&blobs_path)?;
    let staging = DurableDirectory::open(&staging_path)?;
    let blobs = DurableDirectory::open(&blobs_path)?;

    staging
        .create_file("source")?
        .write_all(b"complete payload")?;

    denied_under_mode(&staging_path, 0o500, || {
        staging.create_file("new").map(drop)
    })?;

    assert!(!staging_path.join("new").try_exists()?);

    denied_under_mode(&staging_path.join("source"), 0, || {
        staging.publish_new("source", &blobs, "final")
    })?;

    assert_eq!(staging.faults.trace(), []);
    assert!(!blobs_path.join("final").try_exists()?);

    for directory_path in [&staging_path, &blobs_path] {
        staging.faults.arm(None);

        denied_under_mode(directory_path, 0o500, || {
            staging.publish_new("source", &blobs, "final")
        })?;

        let rename_attempt = PUBLICATION
            .iter()
            .position(|step| *step == (Effect::Rename, Phase::Before))
            .unwrap();

        assert_eq!(staging.faults.trace(), PUBLICATION[..=rename_attempt]);

        assert_eq!(
            std::fs::read(staging_path.join("source"))?,
            b"complete payload"
        );

        assert!(!blobs_path.join("final").try_exists()?);
    }

    staging.publish_new("source", &blobs, "final")?;

    denied_under_mode(&blobs_path, 0o500, || blobs.remove_owned("final"))?;

    assert_eq!(blobs.faults.trace(), [(Effect::Unlink, Phase::Before)]);

    assert_eq!(
        std::fs::read(blobs_path.join("final"))?,
        b"complete payload"
    );

    blobs.remove_owned("final")?;
    assert!(!blobs_path.join("final").try_exists()?);
    temporary.finish()
}

#[cfg(target_os = "macos")]
fn denied_under_mode(
    path: &Path,
    mode: u32,
    attempt: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let original = std::fs::metadata(path)?.permissions();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    let outcome = attempt();
    std::fs::set_permissions(path, original)?;
    assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    Ok(())
}

#[cfg(unix)]
#[test]
fn publication_refuses_symlink_and_directory_sources_before_any_effect() -> io::Result<()> {
    let temporary = directory::TestDirectory::new()?;
    let directory = DurableDirectory::open(temporary.path())?;
    directory.create_file("original")?.write_all(b"sentinel")?;
    std::os::unix::fs::symlink("original", temporary.path().join("staging"))?;

    let error = directory
        .publish_new("staging", &directory, "final")
        .unwrap_err();

    assert_eq!(
        error.raw_os_error(),
        Some(rustix::io::Errno::LOOP.raw_os_error())
    );

    assert!(std::fs::symlink_metadata(temporary.path().join("staging"))?.is_symlink());

    assert_eq!(
        std::fs::read(temporary.path().join("original"))?,
        b"sentinel"
    );

    assert!(!temporary.path().join("final").try_exists()?);
    assert_eq!(directory.faults.trace(), []);

    std::fs::remove_file(temporary.path().join("staging"))?;
    std::fs::create_dir(temporary.path().join("staging"))?;

    let error = directory
        .publish_new("staging", &directory, "final")
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(temporary.path().join("staging").is_dir());
    assert!(!temporary.path().join("final").try_exists()?);
    assert_eq!(directory.faults.trace(), []);
    temporary.finish()
}

#[cfg(any(unix, windows))]
#[test]
fn publication_preserves_an_existing_destination_symlink_and_its_target() -> io::Result<()> {
    let temporary = directory::TestDirectory::new()?;
    let directory = DurableDirectory::open(temporary.path())?;
    directory.create_file("original")?.write_all(b"sentinel")?;

    directory
        .create_file("staging")?
        .write_all(b"new payload")?;

    #[cfg(unix)]
    std::os::unix::fs::symlink("original", temporary.path().join("final"))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("original", temporary.path().join("final"))?;

    let error = directory
        .publish_new("staging", &directory, "final")
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(std::fs::symlink_metadata(temporary.path().join("final"))?.is_symlink());

    assert_eq!(
        std::fs::read(temporary.path().join("original"))?,
        b"sentinel"
    );

    assert_eq!(
        std::fs::read(temporary.path().join("staging"))?,
        b"new payload"
    );

    let rename_attempt = PUBLICATION
        .iter()
        .position(|step| *step == (Effect::Rename, Phase::Before))
        .unwrap();

    assert_eq!(directory.faults.trace(), PUBLICATION[..=rename_attempt]);
    temporary.finish()
}

#[test]
fn publication_faults_preserve_the_real_effect_and_stop_before_later_steps() -> io::Result<()> {
    // A returned error leaves one complete file. The rename acknowledgment is
    // the first checkpoint at which the final path is allowed to exist.
    for (index, (effect, phase)) in PUBLICATION.iter().copied().enumerate() {
        let source_exists = match (effect, phase) {
            (Effect::FileSync, _) | (Effect::Rename, Phase::Before) => true,
            #[cfg(target_os = "macos")]
            (Effect::DeviceSync, _) => true,
            _ => false,
        };

        let temporary = directory::TestDirectory::new()?;
        let source_path = temporary.path().join("staging");
        let destination_path = temporary.path().join("blobs");
        std::fs::create_dir(&source_path)?;
        std::fs::create_dir(&destination_path)?;
        let source = DurableDirectory::open(&source_path)?;
        let destination = DurableDirectory::open(&destination_path)?;
        let mut file = source.create_file("owned")?;
        file.write_all(b"\0complete payload\xff")?;

        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            (metadata.dev(), metadata.ino())
        };

        drop(file);
        source.faults.arm(Some((effect, phase)));

        let error = source
            .publish_new("owned", &destination, "final")
            .unwrap_err();

        assert_eq!(
            error
                .get_ref()
                .and_then(|error| error.downcast_ref::<faults::Failure>()),
            Some(&faults::Failure(effect, phase)),
        );

        assert_eq!(source.faults.trace(), PUBLICATION[..=index]);
        drop(source);
        drop(destination);
        assert_eq!(source_path.join("owned").try_exists()?, source_exists);
        assert_eq!(destination_path.join("final").try_exists()?, !source_exists);

        let retained = if source_exists {
            source_path.join("owned")
        } else {
            destination_path.join("final")
        };

        assert_eq!(std::fs::read(&retained)?, b"\0complete payload\xff");

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(&retained)?;

            assert_eq!(
                (metadata.dev(), metadata.ino()),
                identity,
                "publication copied instead of renaming"
            );
        }

        // Reopening and reading shows the live namespace only; no successful
        // sync or process-level observation is promoted to power-loss evidence.
        DurableDirectory::open(&source_path)?.sync()?;
        DurableDirectory::open(&destination_path)?.sync()?;
        temporary.finish()?;
    }

    Ok(())
}

#[test]
fn lost_unlink_acknowledgment_retries_sync_even_when_the_file_is_absent() -> io::Result<()> {
    for (index, (effect, phase)) in REMOVAL.into_iter().enumerate() {
        let temporary = directory::TestDirectory::new()?;
        let directory = DurableDirectory::open(temporary.path())?;

        directory
            .create_file("owned")?
            .write_all(b"retired payload")?;

        directory.faults.arm(Some((effect, phase)));
        let error = directory.remove_owned("owned").unwrap_err();

        assert_eq!(
            error
                .get_ref()
                .and_then(|error| error.downcast_ref::<faults::Failure>()),
            Some(&faults::Failure(effect, phase)),
        );

        assert_eq!(directory.faults.trace(), REMOVAL[..=index]);
        assert_eq!(temporary.path().join("owned").try_exists()?, index == 0);
        drop(directory);
        let directory = DurableDirectory::open(temporary.path())?;
        directory.remove_owned("owned")?;
        assert!(!temporary.path().join("owned").try_exists()?);
        assert_eq!(directory.faults.trace(), REMOVAL);
        directory.faults.arm(None);
        directory.remove_owned("owned")?;
        assert_eq!(directory.faults.trace(), REMOVAL);
        temporary.finish()?;
    }

    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn cross_device_publication_preserves_source_without_copying() -> io::Result<()> {
    let source = directory::TestDirectory::new()?;
    let destination = directory::TestDirectory::new_in(Path::new("/dev/shm"))?;
    let source_dir = DurableDirectory::open(source.path())?;
    let destination_dir = DurableDirectory::open(destination.path())?;

    source_dir
        .create_file("staging")?
        .write_all(b"must not be copied")?;

    let error = source_dir
        .publish_new("staging", &destination_dir, "visible")
        .unwrap_err();

    assert_eq!(
        error.raw_os_error(),
        Some(rustix::io::Errno::XDEV.raw_os_error())
    );

    assert_eq!(
        std::fs::read(source.path().join("staging"))?,
        b"must not be copied"
    );

    assert!(!destination.path().join("visible").exists());
    source.finish()?;
    destination.finish()
}

#[test]
fn publication_preserves_an_existing_target_and_source() -> io::Result<()> {
    let temporary = directory::TestDirectory::new()?;
    let directory = DurableDirectory::open(temporary.path())?;

    directory
        .create_file("staging")?
        .write_all(b"new payload")?;

    directory
        .create_file("visible")?
        .write_all(b"original payload")?;

    let error = directory
        .publish_new("staging", &directory, "visible")
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

    assert_eq!(
        std::fs::read(temporary.path().join("visible"))?,
        b"original payload"
    );

    assert_eq!(
        std::fs::read(temporary.path().join("staging"))?,
        b"new payload"
    );

    temporary.finish()
}

#[test]
fn publication_moves_exact_bytes_and_owned_unlink_is_repeatable() -> io::Result<()> {
    let temporary = directory::TestDirectory::new()?;
    let directory = DurableDirectory::open(temporary.path())?;

    directory
        .create_file("staging")?
        .write_all(b"\0payload\xff")?;

    directory.publish_new("staging", &directory, "visible")?;
    assert!(!temporary.path().join("staging").exists());

    assert_eq!(
        std::fs::read(temporary.path().join("visible"))?,
        b"\0payload\xff"
    );

    directory.remove_owned("visible")?;
    directory.remove_owned("visible")?;
    assert!(!temporary.path().join("visible").exists());
    temporary.finish()
}

#[test]
fn relative_names_cannot_escape_or_overwrite_files() -> io::Result<()> {
    let temporary = directory::TestDirectory::new()?;
    let directory = DurableDirectory::open(temporary.path())?;

    for name in [
        "",
        ".",
        "..",
        "../outside",
        "/absolute",
        "a/b",
        "a\\b",
        "null\0",
    ] {
        assert_eq!(
            directory.create_file(name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "{name:?}"
        );
    }

    directory.create_file("existing")?.write_all(b"original")?;

    assert_eq!(
        directory.create_file("existing").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );

    assert_eq!(
        std::fs::read(temporary.path().join("existing"))?,
        b"original"
    );

    temporary.finish()
}
