//! Unix descriptor readiness, including terminals unsupported by macOS poll.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::atomic::AtomicBool;

use rustix::event::{FdSetElement, Timespec, fd_set_insert, fd_set_num_elements};
use rustix::fs::{Mode, OFlags};

use super::{POLICY_LIMIT, check_canceled};
use crate::failure::{Failure, Phase};

pub(super) fn read(path: &str, cancellation: &AtomicBool) -> Result<Vec<u8>, Failure> {
    check_canceled(cancellation)?;

    if path == "-" {
        // The executable has a single stdin reader. Retain the lock while using
        // its descriptor so no buffered reader can consume readiness first.
        let stdin = io::stdin();
        let lock = stdin.lock();
        return read_descriptor(lock.as_fd(), cancellation);
    }

    // Nonblocking open also makes a named pipe with no writer interruptible.
    let file = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| Failure::io(Phase::Input, "cannot open policy file"))?;

    read_descriptor(file.as_fd(), cancellation)
}

fn read_descriptor(fd: BorrowedFd<'_>, cancellation: &AtomicBool) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];

    while bytes.len() <= POLICY_LIMIT {
        check_canceled(cancellation)?;

        if !readable(fd).map_err(|_| Failure::io(Phase::Input, "cannot wait for policy input"))? {
            continue;
        }

        let remaining = buffer.len().min(POLICY_LIMIT + 1 - bytes.len());

        match rustix::io::read(fd, &mut buffer[..remaining]) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {}
            Err(_) => return Err(Failure::io(Phase::Input, "cannot read policy input")),
        }
    }

    Ok(bytes)
}

#[allow(unsafe_code, reason = "rustix select requires a live descriptor proof")]
fn readable(fd: BorrowedFd<'_>) -> rustix::io::Result<bool> {
    let bound = fd
        .as_raw_fd()
        .checked_add(1)
        .ok_or(rustix::io::Errno::INVAL)?;

    let mut descriptors = vec![FdSetElement::default(); fd_set_num_elements(1, bound)];
    fd_set_insert(&mut descriptors, fd.as_raw_fd());

    let interval = Timespec {
        tv_sec: 0,
        tv_nsec: 100_000_000,
    };

    // SAFETY: The only set bit names fd, whose borrow keeps its owner alive
    // across this call. The zero-initialized set is sized for bound using
    // rustix's platform-specific sizing helper. No descriptor is closed here.
    let result = unsafe {
        rustix::event::select(bound, Some(&mut descriptors), None, None, Some(&interval))
    };

    match result {
        Ok(count) => Ok(count != 0),
        Err(rustix::io::Errno::INTR) => Ok(false),
        Err(error) => Err(error),
    }
}
