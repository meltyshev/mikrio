//! Bounded policy input with an owned reader that settles before shutdown.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::failure::{Failure, Phase};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

const POLICY_LIMIT: usize = 20 * 1024;

pub(crate) async fn read_policy(
    path: &str,
    cancellation: Arc<AtomicBool>,
) -> Result<String, Failure> {
    let path = path.to_owned();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let requested = Arc::clone(&cancellation);

    let reader = std::thread::Builder::new()
        .name("mikrio-policy-input".to_owned())
        .spawn(move || {
            #[cfg(unix)]
            let result = unix::read(&path, &requested);
            #[cfg(windows)]
            let result = windows::read(&path, &requested);
            let _delivery = sender.send(result);
        })
        .map_err(|_| Failure::io(Phase::Input, "cannot start policy input worker"))?;

    #[cfg(unix)]
    let result = receiver.await;
    #[cfg(windows)]
    let result = windows::settle(receiver, &reader, &cancellation).await;

    reader
        .join()
        .map_err(|_| Failure::io(Phase::Input, "policy input worker failed"))?;

    check_canceled(&cancellation)?;
    let bytes = result.map_err(|_| Failure::io(Phase::Input, "policy input worker failed"))??;

    if bytes.len() > POLICY_LIMIT {
        return Err(invalid_document("policy document exceeds 20 KiB"));
    }

    String::from_utf8(bytes).map_err(|_| invalid_document("policy document must be UTF-8"))
}

fn check_canceled(cancellation: &AtomicBool) -> Result<(), Failure> {
    if cancellation.load(Ordering::Acquire) {
        return Err(Failure::interrupted(Phase::Input));
    }

    Ok(())
}

fn invalid_document(message: &'static str) -> Failure {
    Failure {
        phase: Phase::Input,
        ..Failure::argument(message)
    }
}
