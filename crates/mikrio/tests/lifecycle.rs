use std::fs;
use std::io;

use mikrio::{ErrorCode, OpenOptions};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

#[test]
fn public_lifecycle_preserves_information_and_independent_connections()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");

        let first = stores
            .open(OpenOptions::new(&path).region("eu-west-1"))
            .await?;

        let alias = first.clone();
        let second = stores.open(OpenOptions::new(&path)).await?;
        let info = first.info().clone();
        assert_eq!(info.directory(), path.canonicalize()?);
        assert_eq!(info.region(), "eu-west-1");
        assert_eq!(info.format_version(), 1);
        assert_eq!(info.store_id().len(), 32);
        assert_eq!(first.info(), second.info());
        alias.close().await?;
        first.close().await?;
        second.close().await?;
        let reopened = stores.open(OpenOptions::new(&path)).await?;
        assert_eq!(reopened.info(), &info);
        reopened.close().await?;
        assert_eq!(first.info(), &info);
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn public_errors_are_typed_and_preserve_rejected_contents() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");

        let error = stores
            .open(OpenOptions::new(&path).region("INVALID"))
            .await
            .err()
            .unwrap();

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert_eq!(error.code().as_str(), "InvalidArgument");
        assert!(!path.try_exists()?);
        fs::create_dir(&path)?;
        fs::write(path.join("sentinel"), b"preserve")?;
        let error = stores.open(OpenOptions::new(&path)).await.err().unwrap();
        assert_eq!(error.code(), ErrorCode::InvalidStoreDirectory);
        assert_eq!(fs::read(path.join("sentinel"))?, b"preserve");
        assert_eq!(fs::read_dir(&path)?.count(), 1);
        assert!(std::error::Error::source(&error).is_some());
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn options_construction_has_no_filesystem_side_effects() -> io::Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");
    let _options = OpenOptions::new(&path).region("eu-west-1");
    assert!(!path.try_exists()?);
    directory.finish()
}

#[test]
fn existing_only_open_refuses_missing_and_empty_storage_and_shares_live_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("store");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        for candidate in [&path, &directory.path().join("missing/child")] {
            let error = stores
                .open(OpenOptions::new(candidate).create_if_missing(false))
                .await
                .err()
                .unwrap();

            assert_eq!(error.code(), ErrorCode::InvalidStoreDirectory);
            assert!(!candidate.try_exists()?);
        }

        assert!(!directory.path().join("missing").try_exists()?);
        fs::create_dir(&path)?;

        let error = stores
            .open(OpenOptions::new(&path).create_if_missing(false))
            .await
            .err()
            .unwrap();

        assert_eq!(error.code(), ErrorCode::InvalidStoreDirectory);
        assert_eq!(fs::read_dir(&path)?.count(), 0);

        let created = stores
            .open(OpenOptions::new(&path).region("eu-west-1"))
            .await?;

        let existing = stores
            .open(OpenOptions::new(&path).create_if_missing(false))
            .await?;

        assert_eq!(created.info(), existing.info());
        created.close().await?;
        let owner = existing.get_owner_info().await?;
        existing.close().await?;

        let reopened = stores
            .open(OpenOptions::new(&path).create_if_missing(false))
            .await?;

        assert_eq!(reopened.info(), created.info());
        assert_eq!(reopened.get_owner_info().await?, owner);
        Ok(())
    })?;

    directory.finish()?;
    Ok(())
}

// Linux filesystems support raw byte names; APFS rejects their creation itself.
#[cfg(target_os = "linux")]
#[test]
fn resolved_non_utf8_paths_are_rejected_before_initialization_or_directory_creation()
-> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let directory = TestDirectory::new()?;

    let target = directory
        .path()
        .join(OsString::from_vec(b"non-utf8-\xff".to_vec()));

    fs::create_dir(&target)?;
    let alias = directory.path().join("portable-alias");
    std::os::unix::fs::symlink(&target, &alias)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        for path in [alias.clone(), alias.join("missing/child")] {
            let error = stores.open(OpenOptions::new(path)).await.err().unwrap();
            assert_eq!(error.code(), ErrorCode::InvalidArgument);
            assert_eq!(fs::read_dir(&target)?.count(), 0);
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
