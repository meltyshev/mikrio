//! Register signals before acquisition, retaining in-flight work through settlement.

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct Shutdown {
    requested: bool,
    cancellation: Arc<AtomicBool>,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(windows)]
    interrupt: tokio::signal::windows::CtrlC,
}

impl Shutdown {
    pub(crate) fn register() -> io::Result<Self> {
        Ok(Self {
            requested: false,
            cancellation: Arc::default(),
            #[cfg(unix)]
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            #[cfg(unix)]
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
            #[cfg(windows)]
            interrupt: tokio::signal::windows::ctrl_c()?,
        })
    }

    pub(crate) fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancellation)
    }

    pub(crate) async fn wait(&mut self) {
        if self.requested {
            return;
        }

        #[cfg(unix)]
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }

        #[cfg(windows)]
        self.interrupt.recv().await;

        self.requested = true;
        self.cancellation.store(true, Ordering::Release);
    }

    pub(crate) async fn is_requested(&mut self) -> bool {
        tokio::select! {
            biased;
            () = self.wait() => true,
            () = std::future::ready(()) => false,
        }
    }

    pub(crate) async fn settle<T>(&mut self, work: impl Future<Output = T>) -> T {
        tokio::pin!(work);

        tokio::select! {
            biased;
            () = self.wait() => work.await,
            result = &mut work => result,
        }
    }
}
