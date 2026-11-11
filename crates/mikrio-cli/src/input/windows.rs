//! Cancel only this command's owned synchronous policy reader on Ctrl+C.

use std::fs::File;
use std::io::{self, Read};
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::oneshot;

use super::{POLICY_LIMIT, check_canceled};
use crate::failure::{Failure, Phase};

pub(super) fn read(path: &str, cancellation: &AtomicBool) -> Result<Vec<u8>, Failure> {
    check_canceled(cancellation)?;

    if path == "-" {
        return read_bounded(io::stdin().lock(), cancellation);
    }

    let file =
        File::open(path).map_err(|_| Failure::io(Phase::Input, "cannot open policy file"))?;

    read_bounded(file, cancellation)
}

fn read_bounded(mut reader: impl Read, cancellation: &AtomicBool) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];

    while bytes.len() <= POLICY_LIMIT {
        check_canceled(cancellation)?;
        let remaining = buffer.len().min(POLICY_LIMIT + 1 - bytes.len());

        match reader.read(&mut buffer[..remaining]) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Failure::io(Phase::Input, "cannot read policy input")),
        }
    }

    Ok(bytes)
}

pub(super) async fn settle<T>(
    mut receiver: oneshot::Receiver<T>,
    reader: &JoinHandle<()>,
    cancellation: &AtomicBool,
) -> Result<T, oneshot::error::RecvError> {
    loop {
        tokio::select! {
            result = &mut receiver => return result,
            () = tokio::time::sleep(Duration::from_millis(100)) => {
                if cancellation.load(Ordering::Acquire) {
                    cancel(reader);
                }
            }
        }
    }
}

#[allow(
    unsafe_code,
    reason = "Windows synchronous input has no safe cancellation API"
)]
fn cancel(reader: &JoinHandle<()>) {
    // SAFETY: JoinHandle owns this live kernel thread handle throughout the
    // call. The thread only reads policy input. Cancellation requests completion;
    // it does not terminate the thread or release its buffers. The caller joins
    // the reader before returning. Repetition covers the check-to-read race;
    // ERROR_NOT_FOUND means no synchronous request currently needs cancellation.
    let _ = unsafe { windows_sys::Win32::System::IO::CancelSynchronousIo(reader.as_raw_handle()) };
}
