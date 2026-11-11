//! A foreign context is usable only until explicit unregister drains its single
//! observer thread. Storage close revokes delivery without joining that thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{JoinHandle, ThreadId};
use std::time::Duration;

use crate::error::Error;

pub(crate) struct Observer {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread_id: ThreadId,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Observer {
    pub(crate) fn start(
        receiver: mikrio::embedding::Diagnostics,
        registered: Arc<AtomicBool>,
        closed: Arc<AtomicBool>,
        callback: impl Fn(&str) + Send + 'static,
    ) -> Result<Self, Error> {
        if receiver.is_closed() || closed.load(Ordering::Acquire) {
            return Err(Error::new("StoreClosed", "diagnostic source is closed"));
        }

        // The claim excludes another observer until Registration releases it,
        // including failed thread creation. Acquire pairs with that final release;
        // callback data itself is owned by the thread, not published by this flag.
        registered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::invalid("a diagnostic hook is already registered"))?;

        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let stopped = Arc::clone(&stop);
        let registration = Registration(registered);

        let thread = std::thread::Builder::new()
            .name("mikrio-c-diagnostics".into())
            .spawn(move || {
                let _registration = registration;

                loop {
                    let Ok(stopping) = stopped.0.lock() else {
                        return;
                    };

                    let Ok((stopping, _)) = stopped.1.wait_timeout_while(
                        stopping,
                        Duration::from_millis(250),
                        |stop| !*stop,
                    ) else {
                        return;
                    };

                    if *stopping || receiver.is_closed() || closed.load(Ordering::Acquire) {
                        return;
                    }

                    drop(stopping);

                    if let Some(message) = receiver.take() {
                        // Only detached redacted text crosses the callback. The
                        // observer owns no Store, native work, transaction or guard.
                        let delivered =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                callback(&message);
                            }));

                        if delivered.is_err() {
                            receiver.observer_failed();
                        }
                    }
                }
            })
            .map_err(|_| Error::new("IoError", "diagnostic thread creation failed"))?;

        Ok(Self {
            stop,
            thread_id: thread.thread().id(),
            thread: Mutex::new(Some(thread)),
        })
    }

    pub(crate) fn unregister(&self) -> Result<(), Error> {
        {
            let mut stop = self
                .stop
                .0
                .lock()
                .map_err(|_| Error::new("IoError", "diagnostic stop state is poisoned"))?;

            *stop = true;
        }

        self.stop.1.notify_all();

        if std::thread::current().id() == self.thread_id {
            return Err(Error::invalid(
                "diagnostic unregister cannot join its own callback",
            ));
        }

        let mut thread = self
            .thread
            .lock()
            .map_err(|_| Error::new("IoError", "diagnostic join state is poisoned"))?;

        if let Some(thread) = thread.take() {
            thread
                .join()
                .map_err(|_| Error::new("IoError", "diagnostic worker panicked"))?;
        }

        Ok(())
    }
}

struct Registration(Arc<AtomicBool>);

impl Drop for Registration {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
