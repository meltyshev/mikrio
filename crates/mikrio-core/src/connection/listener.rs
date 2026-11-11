//! Listener ownership shares the connection fence, with separate HTTP cancellation.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::task::Poll;

use tokio::sync::{Notify, watch};

use super::{Connection, ConnectionError, ConnectionOwner, Lifecycle, Operation, shutdown};

pub(super) struct Scope {
    pub(super) closing: watch::Sender<bool>,
    operations: AtomicUsize,
    drained: Notify,
}

impl Scope {
    pub(super) fn admitted(&self) {
        // The enclosing connection's checked count bounds this subset too.
        self.operations.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn settled(&self) {
        if self.operations.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.drained.notify_waiters();
        }
    }

    async fn drain(&self) {
        loop {
            let mut notified = pin!(self.drained.notified());
            notified.as_mut().enable();

            if self.operations.load(Ordering::Acquire) == 0 {
                return;
            }

            notified.await;
        }
    }
}

/// Reserved listener slot and connection-owned transport lifetime. HTTP must use
/// [`Self::connection`] so every native command carries the listener close fence.
/// The transport owner retains this lease until sockets and native work settle.
pub struct ListenerLease {
    connection: Connection,
    scope: Arc<Scope>,
    _lifetime: Operation,
}

/// Cancellation-only handle. It retains no connection or native operation.
#[derive(Clone)]
pub struct ListenerControl(Arc<Scope>);

impl ListenerControl {
    /// Fence this listener's HTTP operations before signaling transport shutdown.
    pub fn close(&self) {
        self.0.closing.send_replace(true);
    }
}

impl Connection {
    /// Reserve the sole listener slot before binding. No socket or task is made.
    ///
    /// # Errors
    /// Refuses a closing connection, an occupied slot or exhausted accounting.
    pub fn reserve_listener(&self) -> Result<ListenerLease, ConnectionError> {
        let lifetime = self.admit()?;

        let mut activity = self
            .owner
            .state
            .activity
            .lock()
            .map_err(|_| shutdown("poisoned listener reservation"))?;

        match activity.lifecycle {
            Lifecycle::Open => {}
            Lifecycle::Closing(_) => return Err(ConnectionError::StoreClosing),
            Lifecycle::Closed => return Err(ConnectionError::StoreClosed),
        }

        if activity.listener.upgrade().is_some() {
            return Err(ConnectionError::ListenerAlreadyRunning);
        }

        let scope = Arc::new(Scope {
            closing: watch::channel(false).0,
            operations: AtomicUsize::new(0),
            drained: Notify::new(),
        });

        activity.listener = Arc::downgrade(&scope);

        Ok(ListenerLease {
            connection: Connection {
                owner: Arc::new(ConnectionOwner {
                    state: Arc::clone(&self.owner.state),
                    listener: Some(Arc::clone(&scope)),
                }),
                info: Arc::clone(&self.info),
            },
            scope,
            _lifetime: lifetime,
        })
    }
}

impl ListenerLease {
    /// A detached close signal that cannot prolong the store's native lifetime.
    #[must_use]
    pub fn control(&self) -> ListenerControl {
        ListenerControl(Arc::clone(&self.scope))
    }

    /// Scoped engine access; clones do not retain a public Store wrapper.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Publish readiness against the same connection and listener close fences.
    ///
    /// # Errors
    /// Refuses shutdown that won publication. The caller still owns teardown.
    pub fn ready(&self) -> Result<(), ConnectionError> {
        self.connection.admit()?.enter_commit().map(drop)
    }

    /// Stop HTTP admission and future commit entry; direct calls remain usable.
    pub fn begin_close(&self) {
        self.scope.closing.send_replace(true);
    }

    /// Wait for either parent or listener shutdown without retaining a mutex.
    pub async fn closing(&self) {
        let mut parent = self.connection.owner.state.closing.subscribe();
        let mut listener = self.scope.closing.subscribe();

        if *parent.borrow() || *listener.borrow() {
            return;
        }

        let mut parent = pin!(parent.changed());
        let mut listener = pin!(listener.changed());

        poll_fn(|cx| {
            if parent.as_mut().poll(cx).is_ready() || listener.as_mut().poll(cx).is_ready() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }

    /// Parent shutdown also drives a failed child attempt without another public
    /// Server call. This observation is independent of the child close signal.
    pub async fn connection_closing(&self) {
        let mut parent = self.connection.owner.state.closing.subscribe();

        if *parent.borrow() {
            return;
        }

        let _closed = parent.changed().await;
    }

    /// After transport teardown, settle actual native work before releasing the
    /// slot. Canceling this wait retains the closed scope and operation owners.
    ///
    /// # Errors
    /// Preserves poisoned ownership as a shutdown failure; never reopens a scope.
    pub async fn finish(&self) -> Result<(), ConnectionError> {
        self.begin_close();
        self.scope.drain().await;

        let mut activity = self
            .connection
            .owner
            .state
            .activity
            .lock()
            .map_err(|_| shutdown("poisoned listener settlement"))?;

        if activity.listener.ptr_eq(&Arc::downgrade(&self.scope)) {
            activity.listener = Weak::new();
        }

        Ok(())
    }
}

impl Drop for ListenerLease {
    fn drop(&mut self) {
        self.begin_close();
        // A failed/canceled start has no transport work. Otherwise native work
        // retains Scope and the weak slot until its real operations settle.
    }
}
