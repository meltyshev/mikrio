//! Actual native sidecar path-length refusal must preserve recognized initialization.

use std::os::unix::fs::MetadataExt as _;

use super::*;

#[test]
fn overlong_sidecar_lookup_preserves_initialization_and_recovers_at_a_shorter_path() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut path = directory.path().canonicalize()?.join("nested");
    fs::create_dir(&path)?;
    let mut observed = None;

    for _ in 0..1024 {
        let result = path.join("metadata.sqlite3-journal").symlink_metadata();

        if let Err(error) = result
            && error.kind() != io::ErrorKind::NotFound
        {
            observed = Some(error);
            break;
        }

        path.push("x");
        fs::create_dir(&path)?;
    }

    let error = observed.expect("owned native path reaches the sidecar length boundary");

    assert_eq!(
        error.raw_os_error(),
        Some(rustix::io::Errno::NAMETOOLONG.raw_os_error())
    );

    for name in [
        "objects",
        "staging",
        "locks",
        "metadata.sqlite3",
        "metadata.sqlite3-wal",
        "metadata.sqlite3-shm",
    ] {
        assert_eq!(
            path.join(name).symlink_metadata().unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    let marker = IdentityMarker::initializing(StoreId::from_bytes([97; 16]));
    File::create_new(path.join(".mikrio.lock"))?;
    fs::write(path.join("store.json"), marker.encode()?)?;
    let inode = path.join(".mikrio.lock").metadata()?.ino();
    let result = OpenedStore::open(&path, Some("eu-west-1"));
    let error = result.err().unwrap();
    assert_eq!(error.failure(), OpenFailure::IoError);

    let cause = std::error::Error::source(&error)
        .unwrap()
        .downcast_ref::<io::Error>()
        .unwrap();

    assert_eq!(
        cause.raw_os_error(),
        Some(rustix::io::Errno::NAMETOOLONG.raw_os_error())
    );

    assert_eq!(fs::read(path.join("store.json"))?, marker.encode()?);
    assert_eq!(path.join(".mikrio.lock").metadata()?.ino(), inode);

    let mut entries = fs::read_dir(&path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;

    entries.sort();
    assert_eq!(entries, [".mikrio.lock", "store.json"]);

    // Moving only this owned fixture clears the real OS path-length condition.
    let recovered_path = directory.path().join("recovered");
    fs::rename(&path, &recovered_path)?;
    let recovered = OpenedStore::open(&recovered_path, Some("eu-west-1"))?;
    assert_eq!(recovered.record().store_id, marker.store_id());
    assert_eq!(recovered.record().region, "eu-west-1");
    close(recovered)?;
    assert_eq!(recovered_path.join(".mikrio.lock").metadata()?.ino(), inode);

    assert_eq!(
        read_marker(&recovered_path.join("store.json"))?.state(),
        MarkerState::Ready
    );

    directory.finish()?;
    Ok(())
}
