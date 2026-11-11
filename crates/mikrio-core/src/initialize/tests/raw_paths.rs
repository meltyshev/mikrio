use super::*;

#[cfg(unix)]
fn nonportable_target(directory: &TestDirectory) -> Result<PathBuf, io::Error> {
    use std::os::unix::ffi::OsStringExt;

    let target = directory
        .path()
        .join(std::ffi::OsString::from_vec(b"target-\xff".to_vec()));

    fs::create_dir(&target)?;
    Ok(target)
}

#[cfg(unix)]
#[test]
fn existing_portable_alias_to_nonportable_root_is_refused_without_writes() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let target = nonportable_target(&directory)?;
    let alias = directory.path().join("portable-alias");
    std::os::unix::fs::symlink(&target, &alias)?;
    assert!(alias.to_str().is_some());
    assert!(alias.canonicalize()?.to_str().is_none());

    assert_eq!(failure(&alias, None), OpenFailure::InvalidArgument);
    assert_eq!(fs::read_link(&alias)?, target);
    assert!(fs::read_dir(&target)?.next().transpose()?.is_none());
    fs::remove_file(alias)?;
    fs::remove_dir(target)?;
    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn portable_alias_to_nonportable_parent_is_refused_before_child_creation() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let target = nonportable_target(&directory)?;
    let alias = directory.path().join("portable-alias");
    std::os::unix::fs::symlink(&target, &alias)?;
    let absent = alias.join("parent/store");
    assert!(absent.to_str().is_some());
    assert!(!absent.try_exists()?);

    assert_eq!(failure(&absent, None), OpenFailure::InvalidArgument);
    assert_eq!(fs::read_link(&alias)?, target);
    assert!(fs::read_dir(&target)?.next().transpose()?.is_none());
    assert!(!target.join("parent").try_exists()?);
    fs::remove_file(alias)?;
    fs::remove_dir(target)?;
    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn relative_portable_path_from_nonportable_cwd_is_refused_before_creation() -> TestResult {
    use std::os::unix::fs::MetadataExt as _;

    let _scenario = crate::test_process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let target = nonportable_target(&directory)?;
    let before = fs::metadata(&target)?;

    let mut child = crate::test_process::Probe::start_configured(
        &target,
        "relative-nonportable-cwd",
        "initialize::tests::raw_paths::relative_path_child",
        |command| {
            command.current_dir(&target);
            Ok(())
        },
    )?;

    assert_eq!(child.observation()?, "REFUSED_WITHOUT_CREATION");
    assert!(fs::read_dir(&target)?.next().transpose()?.is_none());
    child.finish()?;

    let after = fs::metadata(&target)?;
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert!(fs::read_dir(&target)?.next().transpose()?.is_none());
    fs::remove_dir(target)?;
    directory.finish()?;
    Ok(())
}

#[cfg(unix)]
#[test]
#[ignore = "Owned subprocess entry for relative paths from a non-UTF8 working directory"]
fn relative_path_child() -> TestResult {
    use std::io::BufRead as _;

    let expected = std::env::var_os("mikrio_probe_directory").ok_or("missing owned directory")?;

    assert_eq!(
        std::env::var("mikrio_probe_mode")?,
        "relative-nonportable-cwd"
    );

    let cwd = std::env::current_dir()?;
    assert_eq!(cwd, Path::new(&expected).canonicalize()?);
    assert!(cwd.to_str().is_none());
    let relative = Path::new("portable-child");
    assert!(relative.to_str().is_some());

    assert_eq!(failure(relative, None), OpenFailure::InvalidArgument);
    assert!(fs::read_dir(&cwd)?.next().transpose()?.is_none());
    let mut output = io::stdout().lock();
    writeln!(output, "PROBE REFUSED_WITHOUT_CREATION")?;
    output.flush()?;
    drop(output);

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    assert_eq!(line, "exit\n");
    Ok(())
}

#[cfg(unix)]
#[test]
fn nonportable_managed_child_preserves_recognized_initialization_and_recovery() -> TestResult {
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::MetadataExt as _;

    let directory = TestDirectory::new()?.retain_on_failure();

    for name in ["objects", "staging", "locks"] {
        let path = directory.path().join(format!("store-{name}"));
        let marker = IdentityMarker::initializing(StoreId::from_bytes([71; 16]));
        partial(&path, &marker)?;
        let managed = path.join(name);
        fs::create_dir(&managed)?;
        let child = managed.join(std::ffi::OsString::from_vec(b"ab\xff".to_vec()));
        fs::write(&child, b"preserve unknown raw child")?;
        assert!(child.file_name().unwrap().to_str().is_none());
        let guard_before = fs::metadata(path.join(".mikrio.lock"))?;
        let child_before = fs::metadata(&child)?;
        let marker_bytes = fs::read(path.join("store.json"))?;

        assert_eq!(failure(&path, None), OpenFailure::StoreCorrupt, "{name}");
        assert_eq!(fs::read(&child)?, b"preserve unknown raw child");
        assert_eq!(fs::read(path.join("store.json"))?, marker_bytes);
        assert_eq!(fs::read(path.join(".mikrio.lock"))?, b"");
        assert_eq!(read_marker(&path.join("store.json"))?, marker);
        assert_eq!(fs::read_dir(&path)?.count(), 3);
        assert_eq!(fs::read_dir(&managed)?.count(), 1);
        let guard_after = fs::metadata(path.join(".mikrio.lock"))?;
        let child_after = fs::metadata(&child)?;

        assert_eq!(
            (guard_after.dev(), guard_after.ino()),
            (guard_before.dev(), guard_before.ino())
        );

        assert_eq!(
            (child_after.dev(), child_after.ino()),
            (child_before.dev(), child_before.ino())
        );

        fs::remove_file(child)?;
        let recovered = OpenedStore::open(&path, Some("eu-west-1"))?;
        assert_eq!(recovered.record().store_id, marker.store_id());
        assert_eq!(recovered.record().region, "eu-west-1");
        assert_eq!(read_marker(&path.join("store.json"))?, marker.ready());
        close(recovered)?;
    }

    directory.finish()?;
    Ok(())
}
