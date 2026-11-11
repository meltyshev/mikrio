//! Real macOS permission failures preserve initializer inputs and typed causes.

use std::cell::RefCell;
use std::error::Error as _;
use std::os::unix::fs::PermissionsExt as _;

use super::*;
use crate::test_backup::manifest;

struct RestrictedDirectory {
    path: PathBuf,
    original: Option<fs::Permissions>,
}

impl RestrictedDirectory {
    fn new(path: &Path, mode: u32) -> io::Result<Self> {
        let original = fs::metadata(path)?.permissions();

        let restriction = Self {
            path: path.to_owned(),
            original: Some(original),
        };

        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(restriction)
    }

    fn restore(&mut self) -> io::Result<()> {
        if let Some(original) = &self.original {
            fs::set_permissions(&self.path, original.clone())?;
            self.original = None;
        }

        Ok(())
    }
}

impl Drop for RestrictedDirectory {
    fn drop(&mut self) {
        report_restore(self.restore());
    }
}

#[allow(
    clippy::print_stderr,
    reason = "a failed permission restore must remain visible beside the primary test failure"
)]
fn report_restore(result: io::Result<()>) {
    if let Err(error) = result {
        eprintln!("owned test permission restore failed: {error}");
    }
}

struct PendingRestriction {
    point: &'static str,
    path: PathBuf,
    restriction: Option<RestrictedDirectory>,
}

thread_local! {
    static PENDING: RefCell<Option<PendingRestriction>> = const { RefCell::new(None) };
}

struct AtEffect {
    _callback: fault::Scope,
    active: bool,
}

impl AtEffect {
    fn new(point: &'static str, path: PathBuf) -> Self {
        PENDING.with(|pending| {
            assert!(pending.borrow().is_none());

            *pending.borrow_mut() = Some(PendingRestriction {
                point,
                path,
                restriction: None,
            });
        });

        Self {
            _callback: fault::install(restrict_at_effect),
            active: true,
        }
    }

    fn restore(&mut self) -> io::Result<bool> {
        if !self.active {
            return Ok(false);
        }

        self.active = false;
        let state = PENDING.with(|pending| pending.borrow_mut().take());

        let Some(mut state) = state else {
            return Ok(false);
        };

        let Some(mut restriction) = state.restriction.take() else {
            return Ok(false);
        };

        restriction.restore()?;
        Ok(true)
    }
}

impl Drop for AtEffect {
    fn drop(&mut self) {
        report_restore(self.restore().map(|_| ()));
    }
}

fn restrict_at_effect(point: &str) -> io::Result<()> {
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();

        let Some(state) = pending.as_mut() else {
            return Ok(());
        };

        if point == state.point && state.restriction.is_none() {
            state.restriction = Some(RestrictedDirectory::new(&state.path, 0o500)?);
        }

        Ok(())
    })
}

fn assert_permission_denied(error: &OpenError) {
    assert_eq!(error.failure(), OpenFailure::IoError);

    assert_eq!(
        error
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );

    assert!(!error.can_recheck_initialization());
}

#[test]
fn unwritable_parent_refuses_creation_without_changing_existing_contents() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let parent = directory.path().join("parent");
    fs::create_dir(&parent)?;
    fs::write(parent.join("sentinel"), b"preserve existing parent bytes")?;
    let before = manifest(&parent)?;
    let path = parent.join("missing/store");
    let mut restriction = RestrictedDirectory::new(&parent, 0o500)?;

    let result = OpenedStore::open(&path, None);
    restriction.restore()?;
    assert_permission_denied(&result.err().unwrap());
    assert_eq!(manifest(&parent)?, before);
    assert!(!parent.join("missing").try_exists()?);

    close(OpenedStore::open(&path, None)?)?;

    assert_eq!(
        fs::read(parent.join("sentinel"))?,
        b"preserve existing parent bytes"
    );

    directory.finish()?;
    Ok(())
}

#[test]
fn partial_validation_preserves_inputs_when_parent_search_is_denied() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let marker = IdentityMarker::initializing(StoreId::from_bytes([91; 16]));
    partial(&path, &marker)?;
    let before = manifest(&path)?;
    let mut restriction = RestrictedDirectory::new(&path, 0o600)?;

    // The explicit validation boundary lets the actual metadata lookup fail;
    // a complete open would first fail its earlier directory inspection.
    let result = validate_partial(&path);
    restriction.restore()?;
    assert_permission_denied(&result.unwrap_err());
    assert_eq!(manifest(&path)?, before);

    let store = OpenedStore::open(&path, None)?;
    assert_eq!(store.record().store_id, marker.store_id());
    close(store)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn denied_managed_directory_creation_retains_recognized_initialization() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let mut effect = AtEffect::new("bootstrap.layout.parent_create.after", path.clone());

    let result = OpenedStore::open(&path, None);
    assert!(effect.restore()?);
    drop(effect);
    assert_permission_denied(&result.err().unwrap());
    let marker = read_marker(&path.join("store.json"))?;
    assert_eq!(marker.state(), MarkerState::Initializing);
    assert!(path.join("objects").is_dir());
    assert_eq!(fs::read_dir(path.join("objects"))?.count(), 0);
    assert!(!path.join("staging").try_exists()?);
    assert!(!path.join("locks").try_exists()?);

    let store = OpenedStore::open(&path, None)?;
    assert_eq!(store.record().store_id, marker.store_id());
    close(store)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn denied_shard_guard_creation_never_publishes_ready_and_can_resume() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let mut effect = AtEffect::new("bootstrap.layout.parent_sync.after", path.join("locks"));

    let result = OpenedStore::open(&path, None);
    assert!(effect.restore()?);
    drop(effect);
    assert_permission_denied(&result.err().unwrap());
    let marker = read_marker(&path.join("store.json"))?;
    assert_eq!(marker.state(), MarkerState::Initializing);
    assert_eq!(fs::read_dir(path.join("locks"))?.count(), 0);
    assert_eq!(fs::read_dir(path.join("objects"))?.count(), 256);
    assert_eq!(fs::read_dir(path.join("staging"))?.count(), 256);

    let store = OpenedStore::open(&path, None)?;
    assert_eq!(store.record().store_id, marker.store_id());
    close(store)?;
    assert_eq!(fs::read_dir(path.join("locks"))?.count(), 256);
    directory.finish()?;
    Ok(())
}

#[test]
fn ready_layout_permission_error_preserves_identity_and_all_persistent_bytes() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");
    let store = OpenedStore::open(&path, Some("eu-west-1"))?;
    let identity = store.record().clone();
    close(store)?;
    let before = manifest(&path)?;
    let mut restriction = RestrictedDirectory::new(&path.join("objects"), 0o600)?;

    let result = OpenedStore::open(&path, None);
    restriction.restore()?;
    assert_permission_denied(&result.err().unwrap());
    assert_eq!(manifest(&path)?, before);

    let reopened = OpenedStore::open(&path, None)?;
    assert_eq!(reopened.record(), &identity);
    close(reopened)?;
    directory.finish()?;
    Ok(())
}
