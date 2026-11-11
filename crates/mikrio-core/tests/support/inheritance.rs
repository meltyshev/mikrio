//! An explicit pre-exec barrier distinguishes descriptor lifetime from Rust ownership.

use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::time::Duration;

use mikrio_core::{GuardManager, GuardMode, GuardTarget};

use super::support::{Probe, TestDirectory, process_scenario};

#[test]
fn forked_descriptor_delays_release_only_until_exec_closes_it() -> io::Result<()> {
    let _scenario = process_scenario()?;
    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;

    let holder = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .ok_or_else(|| io::Error::other("initial guard was busy"))?;

    let (mut parent, child) = UnixStream::pair()?;
    parent.set_read_timeout(Some(Duration::from_secs(5)))?;
    parent.set_write_timeout(Some(Duration::from_secs(5)))?;
    child.set_read_timeout(Some(Duration::from_secs(5)))?;
    child.set_write_timeout(Some(Duration::from_secs(5)))?;

    std::thread::scope(|scope| -> io::Result<()> {
        let path = directory.path();

        let launch = scope.spawn(move || {
            Probe::start_configured(path, "barrier", "guard_child", |command| {
                pre_exec_barrier(command, child);
                Ok(())
            })
        });

        let mut message = [0];
        parent.read_exact(&mut message)?;
        assert_eq!(message, [1]);

        drop(holder);

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_none(),
            "the child still owns the inherited open-file description"
        );

        parent.write_all(&[2])?;

        let mut probe = launch
            .join()
            .map_err(|_| io::Error::other("guard probe launcher panicked"))??;

        assert_eq!(probe.observation()?, "EXEC");

        let acquired = manager
            .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
            .ok_or_else(|| io::Error::other("exec retained the inherited guard"))?;

        drop(acquired);
        probe.finish()
    })?;

    directory.finish()
}

#[allow(unsafe_code, reason = "test-only controlled fork/exec boundary")]
fn pre_exec_barrier(command: &mut std::process::Command, channel: UnixStream) {
    // SAFETY: the child uses only read/write syscalls on a pre-opened owned
    // descriptor and stack bytes. There is no allocation, lock, logging, panic,
    // runtime or inherited store access. Kernel socket timeouts bound failure
    // cleanup even when the child temporarily retains both socket endpoints.
    unsafe {
        command.pre_exec(move || {
            loop {
                match rustix::io::write(&channel, &[1]) {
                    Ok(1) => break,
                    Err(rustix::io::Errno::INTR) => {}
                    Err(error) => return Err(error.into()),
                    _ => return Err(io::ErrorKind::WriteZero.into()),
                }
            }

            let mut message = [0];

            loop {
                match rustix::io::read(&channel, &mut message) {
                    Ok(1) if message == [2] => return Ok(()),
                    Err(rustix::io::Errno::INTR) => {}
                    Err(error) => return Err(error.into()),
                    _ => return Err(io::ErrorKind::UnexpectedEof.into()),
                }
            }
        });
    }
}
