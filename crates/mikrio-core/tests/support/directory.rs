//! Owned temporary directories for native tests, never pre-existing user paths.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

pub(crate) struct TestDirectory {
    path: Option<PathBuf>,
    retain_on_failure: bool,
}

impl TestDirectory {
    pub(crate) fn new() -> io::Result<Self> {
        Self::new_in(&std::env::temp_dir())
    }

    pub(crate) fn new_in(parent: &Path) -> io::Result<Self> {
        let path = create_unique_directory(parent, &NEXT_DIRECTORY)?;

        let directory = Self {
            path: Some(path),
            retain_on_failure: false,
        };

        fs::create_dir(directory.path().join("locks"))?;

        for name in [
            ".mikrio.lock",
            "locks/01.lock",
            "locks/02.lock",
            "locks/03.lock",
        ] {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.path().join(name))?;
        }

        Ok(directory)
    }

    #[allow(
        clippy::expect_used,
        reason = "test owner cannot be accessed after consuming finish"
    )]
    pub(crate) fn path(&self) -> &Path {
        self.path.as_deref().expect("owned test directory")
    }

    pub(crate) fn finish(mut self) -> io::Result<()> {
        fs::remove_dir_all(self.path())?;
        self.path = None;
        Ok(())
    }

    #[allow(
        dead_code,
        reason = "only recovery drills retain successful directories"
    )]
    pub(crate) fn preserve(mut self) -> PathBuf {
        let path = self.path().to_path_buf();
        self.path = None;
        path
    }

    #[allow(
        dead_code,
        reason = "fault suites retain failed stores; ordinary probes clean up"
    )]
    pub(crate) fn retain_on_failure(mut self) -> Self {
        self.retain_on_failure = true;
        self
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            if self.retain_on_failure {
                report_retained(&path);
            } else {
                report_cleanup(fs::remove_dir_all(path));
            }
        }
    }
}

fn create_unique_directory(parent: &Path, next_directory: &AtomicU64) -> io::Result<PathBuf> {
    loop {
        let path = parent.join(format!(
            "mikrio-probe-{}-{}",
            std::process::id(),
            next_directory.fetch_add(1, Ordering::Relaxed)
        ));

        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            // A reused PID can name retained evidence from an earlier process.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
}

#[allow(
    clippy::print_stderr,
    reason = "fault failures must identify preserved owned evidence"
)]
fn report_retained(path: &Path) {
    eprintln!("retained failed test directory: {}", path.display());
}

#[allow(
    clippy::print_stderr,
    reason = "test cleanup failures must remain visible beside the primary failure"
)]
fn report_cleanup(result: io::Result<()>) {
    if let Err(error) = result {
        eprintln!("test resource cleanup failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupied_test_directory_names_preserve_existing_entries() -> io::Result<()> {
        let directory = TestDirectory::new()?;
        let prefix = format!("mikrio-probe-{}-", std::process::id());
        let retained = directory.path().join(format!("{prefix}0"));
        fs::create_dir(&retained)?;
        fs::write(retained.join("evidence"), b"retained directory")?;
        let occupied_file = directory.path().join(format!("{prefix}1"));
        fs::write(&occupied_file, b"retained file")?;

        let created = create_unique_directory(directory.path(), &AtomicU64::new(0))?;

        assert_eq!(created, directory.path().join(format!("{prefix}2")));
        assert!(created.is_dir());
        assert_eq!(fs::read(retained.join("evidence"))?, b"retained directory");
        assert_eq!(fs::read(occupied_file)?, b"retained file");
        directory.finish()
    }

    #[test]
    fn missing_parent_is_reported_without_creating_or_removing_entries() -> io::Result<()> {
        let directory = TestDirectory::new()?;
        let parent = directory.path().join("missing");
        let sentinel = directory.path().join("evidence");
        fs::write(&sentinel, b"retained evidence")?;

        let Err(error) = create_unique_directory(&parent, &AtomicU64::new(0)) else {
            return Err(io::Error::other("missing parent unexpectedly accepted"));
        };

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!parent.exists());
        assert_eq!(fs::read(sentinel)?, b"retained evidence");
        directory.finish()
    }
}
