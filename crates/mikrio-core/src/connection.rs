//! Bounded metadata ownership per actual directory and caller runtime lifetime.
//!
//! The worker alone owns SQLite and store-use protection. Host futures own queue
//! admission/replies, never the native resources of an already submitted command.

mod progress;
pub use progress::WorkProgress;

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::runtime::{Handle, Id};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot, watch};

use crate::guards::DirectoryIdentity;
use crate::{GuardManager, OpenError, OpenFailure, OpenedStore, StoreRecord};

mod access;
mod admin;
mod bucket_configuration;
mod buckets;
mod deletion;
mod lifecycle;
mod listener;
mod listing;
mod maintenance;
mod multipart;
mod objects;
mod tags;

pub use listener::{ListenerControl, ListenerLease};

const QUEUE_CAPACITY: usize = 32;
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_millis(10);
static PREPARATIONS: Semaphore = Semaphore::const_new(4);
type Registry = HashMap<(Id, DirectoryIdentity), Weak<Context>>;
static CONTEXTS: OnceLock<Mutex<Registry>> = OnceLock::new();

/// Native lifecycle failure. The facade translates this internal vocabulary.
#[derive(Debug, Clone)]
pub enum ConnectionError {
    /// Opening failed before a handle was published.
    Open(Arc<OpenError>),
    /// The connection's admission fence has closed.
    StoreClosing,
    /// This connection has completed shutdown.
    StoreClosed,
    /// A listener is already starting, running or settling on this connection.
    ListenerAlreadyRunning,
    /// Listener input, runtime setup or socket acquisition failed before readiness.
    ListenerIo(Arc<io::Error>),
    /// Shutdown failed, with ownership retained where native close returned it.
    ShutdownFailed(Arc<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(error) => error.fmt(formatter),
            Self::StoreClosing => formatter.write_str("store connection is closing"),
            Self::StoreClosed => formatter.write_str("store connection is closed"),
            Self::ListenerAlreadyRunning => formatter.write_str("listener already running"),
            Self::ListenerIo(error) => error.fmt(formatter),
            Self::ShutdownFailed(_) => formatter.write_str("store shutdown failed"),
        }
    }
}

impl std::error::Error for ConnectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open(error) => Some(error.as_ref()),
            Self::ShutdownFailed(error) => Some(error.as_ref()),
            Self::ListenerIo(error) => Some(error.as_ref()),
            Self::StoreClosing | Self::StoreClosed | Self::ListenerAlreadyRunning => None,
        }
    }
}

impl From<OpenError> for ConnectionError {
    fn from(error: OpenError) -> Self {
        Self::Open(Arc::new(error))
    }
}

/// Immutable information remains readable after the owning connection closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    /// Resolved absolute UTF-8 directory.
    pub directory: PathBuf,
    /// Validated database identity/configuration, with no secret fields.
    pub record: StoreRecord,
}

/// One logical connection. Clones alias its admission and shutdown state.
/// Independent opens share only compatible metadata execution resources.
#[derive(Clone)]
pub struct Connection {
    owner: Arc<ConnectionOwner>,
    info: Arc<ConnectionInfo>,
}

// Only public connection aliases retain this owner. Operation tickets retain
// State directly, so ongoing work cannot defer the last-wrapper shutdown fence.
struct ConnectionOwner {
    state: Arc<State>,
    // HTTP aliases retain native resources without counting as Store wrappers.
    listener: Option<Arc<listener::Scope>>,
}

impl Drop for ConnectionOwner {
    fn drop(&mut self) {
        if self.listener.is_some() {
            return;
        }

        if let Ok(mut activity) = self.state.activity.lock()
            && matches!(activity.lifecycle, Lifecycle::Open)
        {
            activity.lifecycle = Lifecycle::Closing(None);
        }

        self.state.closing.send_replace(true);
        // Existing operations still own State/Context and native protection.
        // Their settlement wakes the worker to release abandoned ownership.
    }
}

struct State {
    context: Arc<Context>,
    activity: Mutex<Activity>,
    drained: Notify,
    closing: watch::Sender<bool>,
}

impl Drop for State {
    fn drop(&mut self) {
        // A closed independent handle can still own Context. Wake its worker
        // when this connection's last native operation settles. Never block:
        // a full queue already guarantees another liveness check on the worker.
        let _notification = self.context.sender.try_send(Command::Reap);
    }
}

struct Activity {
    lifecycle: Lifecycle,
    operations: usize,
    listener: Weak<listener::Scope>,
}

enum Lifecycle {
    Open,
    Closing(Option<watch::Receiver<Option<CloseResult>>>),
    Closed,
}

/// Connection-owned work admission. Transfer this owner into the actual native
/// task; abandoning its caller must not drop the task's admission prematurely.
pub struct Operation {
    state: Arc<State>,
    cancellation: watch::Sender<bool>,
    listener: Option<Arc<listener::Scope>>,
    progress: Option<WorkProgress>,
}

// The awaiting caller owns cancellation; the submitted command owns Operation.
// The watch read lock and connection fence order cancellation against commit entry.
struct CancelOnDrop(watch::Sender<bool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

/// Permission to finish an already entered commit section across close. This
/// protects lifecycle ordering only; it does not authorize data or prove commit.
pub struct CommitSection {
    _operation: Operation,
}

impl Operation {
    fn guards(&self, path: &std::path::Path) -> Result<GuardManager, crate::OperationError> {
        let guards = GuardManager::open(path)?;
        self.check_directory(guards.identity())?;
        Ok(guards)
    }

    fn directory(
        &self,
        path: &std::path::Path,
    ) -> Result<crate::DurableDirectory, crate::OperationError> {
        let directory = crate::DurableDirectory::open(path)?;
        self.check_directory(directory.identity()?)?;
        Ok(directory)
    }

    fn check_directory(&self, identity: DirectoryIdentity) -> Result<(), crate::OperationError> {
        if identity != self.state.context.identity {
            return Err(crate::OperationError::caused(
                crate::OperationFailure::StoreCorrupt,
                io::Error::new(io::ErrorKind::InvalidData, "store directory was replaced"),
            ));
        }

        Ok(())
    }

    fn progressed(&self) {
        if let Some(progress) = &self.progress {
            progress.advance();
        }
    }

    fn cancel_on_drop(&self) -> CancelOnDrop {
        CancelOnDrop(self.cancellation.clone())
    }

    fn enter_mutation(self) -> Result<CommitSection, crate::OperationError> {
        self.check_commit()?;
        Ok(CommitSection { _operation: self })
    }

    fn check_commit(&self) -> Result<(), crate::OperationError> {
        {
            let activity = self
                .state
                .activity
                .lock()
                .map_err(|_| shutdown("poisoned operation fence"))?;

            let canceled = self.cancellation.borrow();
            let listener_closing = self.listener.as_ref().map(|scope| scope.closing.borrow());

            if *canceled || listener_closing.as_ref().is_some_and(|closing| **closing) {
                return Err(crate::OperationError::new(
                    crate::OperationFailure::OperationCanceled,
                ));
            }

            match activity.lifecycle {
                Lifecycle::Open => {}
                Lifecycle::Closing(_) => return Err(ConnectionError::StoreClosing.into()),
                Lifecycle::Closed => return Err(ConnectionError::StoreClosed.into()),
            }
            // Both fences are held at this decision. Later cancellation/close
            // can no longer revoke this section; the native command settles it.
        }

        Ok(())
    }

    /// Enter the commit side of the close fence without retaining a mutex.
    /// The returned owner must survive until the native transaction settles.
    ///
    /// # Errors
    /// Rejects a close that won the fence. No storage effect occurs here.
    pub fn enter_commit(self) -> Result<CommitSection, ConnectionError> {
        {
            let activity = self
                .state
                .activity
                .lock()
                .map_err(|_| shutdown("poisoned operation fence"))?;

            let listener_closing = self.listener.as_ref().map(|scope| scope.closing.borrow());

            if listener_closing.as_ref().is_some_and(|closing| **closing) {
                return Err(ConnectionError::StoreClosing);
            }

            match activity.lifecycle {
                Lifecycle::Open => {}
                Lifecycle::Closing(_) => return Err(ConnectionError::StoreClosing),
                Lifecycle::Closed => return Err(ConnectionError::StoreClosed),
            }
        }

        Ok(CommitSection { _operation: self })
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if let Some(scope) = &self.listener {
            scope.settled();
        }

        let drained = if let Ok(mut activity) = self.state.activity.lock() {
            // Only successful admission constructs this owner, exactly once.
            let Some(remaining) = activity.operations.checked_sub(1) else {
                return;
            };

            activity.operations = remaining;
            remaining == 0
        } else {
            false
        };

        if drained {
            self.state.drained.notify_waiters();
        }
    }
}

#[derive(Clone)]
enum CloseResult {
    Finished(Result<(), ConnectionError>),
    Retry,
}

struct Context {
    identity: DirectoryIdentity,
    sender: mpsc::Sender<Command>,
    terminal: watch::Receiver<Option<Result<(), ConnectionError>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    maintenance: Mutex<Option<tokio::task::JoinHandle<()>>>,
    connections: Mutex<Vec<Weak<State>>>,
    wake_maintenance: Arc<Notify>,
    diagnostics: crate::diagnostics::Diagnostics,
    store_id: OnceLock<crate::StoreId>,
    sink: crate::diagnostics::TracingSink,
    #[cfg(test)]
    test_hooks: Arc<TestHooks>,
}

impl Drop for Context {
    fn drop(&mut self) {
        self.diagnostics.close();
    }
}

enum Command {
    Reap,
    Access(access::Command),
    Admin(admin::Command),
    Bucket(buckets::Command),
    Listing(listing::Command),
    BucketConfiguration(bucket_configuration::Command),
    Lifecycle(lifecycle::Command),
    Multipart(multipart::Command),
    Object(objects::Command),
    DeleteBatch(deletion::Command),
    Tags(tags::Command),
    Maintenance(maintenance::Command),
    Open {
        state: Arc<State>,
        region: Option<String>,
        mode: crate::OpenMode,
        reply: oneshot::Sender<Result<ConnectionInfo, ConnectionError>>,
    },
    Close(CloseAttempt),
    #[cfg(test)]
    Probe {
        _state: Arc<State>,
        work: ProbeWork,
        reply: oneshot::Sender<io::Result<()>>,
    },
}

#[cfg(test)]
type ProbeWork = Box<dyn FnOnce(&mut OpenedStore) -> io::Result<()> + Send>;

#[cfg(test)]
type OpenHook = Box<dyn FnOnce(&ConnectionInfo) + Send>;

// Unit-test instrumentation only. No feature, environment switch or exported
// fault API can enable these pauses/failures in an ordinary library artifact.
#[cfg(test)]
#[derive(Default)]
struct TestHooks {
    open_attempts: std::sync::atomic::AtomicUsize,
    fail_close: std::sync::atomic::AtomicBool,
    pause_maintenance: std::sync::atomic::AtomicBool,
    before_open_reply: Mutex<Option<OpenHook>>,
    after_read_selection: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    after_read_guard: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    copy_outputs: Mutex<std::collections::BTreeMap<[u8; 16], crate::BlobId>>,
    put_outputs: Mutex<std::collections::VecDeque<crate::BlobId>>,
}

// Until queued, the close future owns this attempt. Its Drop wakes concurrent
// closers to retry admission, without reopening the connection. Once queued,
// the worker owns it, including after every caller abandons its wait.
struct CloseAttempt {
    state: Arc<State>,
    result: Option<watch::Sender<Option<CloseResult>>>,
}

impl CloseAttempt {
    fn finish(mut self, result: Result<(), ConnectionError>) {
        if let Ok(mut activity) = self.state.activity.lock() {
            activity.lifecycle = if result.is_ok() {
                Lifecycle::Closed
            } else {
                Lifecycle::Closing(None)
            };
        }

        if let Some(sender) = self.result.take() {
            sender.send_replace(Some(CloseResult::Finished(result)));
        }
    }
}

impl Drop for CloseAttempt {
    fn drop(&mut self) {
        let Some(sender) = self.result.take() else {
            return;
        };

        if let Ok(mut activity) = self.state.activity.lock() {
            activity.lifecycle = Lifecycle::Closing(None);
        }

        sender.send_replace(Some(CloseResult::Retry));
    }
}

impl Connection {
    /// Open using the caller's Tokio runtime (with its time driver enabled).
    /// No runtime is created or shut down by core. Filesystem preparation is
    /// bounded; all SQLite/initialization work belongs to the metadata worker.
    ///
    /// # Errors
    /// Refuses invalid/corrupt/unsupported state, mismatched region, missing
    /// caller runtime, and contention exceeding the finite admission budget.
    pub async fn open(directory: PathBuf, region: Option<String>) -> Result<Self, ConnectionError> {
        Self::open_with_mode(directory, region, crate::OpenMode::CreateIfMissing).await
    }

    /// Open with the shared initializer's explicit creation policy.
    ///
    /// # Errors
    /// Preserves [`Self::open`] failures and refuses uninitialized storage in
    /// existing-only mode, including on worker retries and context recreation.
    pub async fn open_with_mode(
        directory: PathBuf,
        region: Option<String>,
        mode: crate::OpenMode,
    ) -> Result<Self, ConnectionError> {
        // Capture process cwd before yielding to queue admission or native work.
        let directory = OpenedStore::absolute(&directory, region.as_deref())?;

        let runtime = Handle::try_current()
            .map_err(|error| OpenError::caused(OpenFailure::InvalidArgument, error))?;

        let deadline = tokio::time::Instant::now() + ADMISSION_TIMEOUT;

        let permit = loop {
            if let Ok(permit) = PREPARATIONS.try_acquire() {
                break permit;
            }

            retry(deadline).await?;
        };

        let prepared = runtime
            .spawn_blocking(move || {
                let _permit = permit;
                let path = OpenedStore::prepare_with_mode(&directory, region.as_deref(), mode)?;
                let identity = GuardManager::open(&path)?.identity();
                Ok::<_, OpenError>((path, identity, region))
            })
            .await
            .map_err(|error| OpenError::caused(OpenFailure::IoError, error))??;

        let (path, identity, region) = prepared;
        let mut shared = context(runtime.id(), identity, &path)?;

        loop {
            if shared.sender.is_closed() {
                shared = context(runtime.id(), identity, &path)?;
            }

            let state = Arc::new(State {
                context: Arc::clone(&shared),
                activity: Mutex::new(Activity {
                    lifecycle: Lifecycle::Open,
                    operations: 0,
                    listener: Weak::new(),
                }),
                drained: Notify::new(),
                closing: watch::channel(false).0,
            });

            let (reply, response) = oneshot::channel();

            let command = Command::Open {
                state: Arc::clone(&state),
                region: region.clone(),
                mode,
                reply,
            };

            match tokio::time::timeout_at(deadline, shared.sender.reserve()).await {
                Ok(Ok(permit)) => permit.send(command),
                Ok(Err(_)) => {
                    retry(deadline).await?;
                    continue;
                }
                Err(_) => return Err(OpenError::new(OpenFailure::StoreBusy).into()),
            }

            match response.await {
                Ok(Ok(info)) => {
                    let _identity = shared.store_id.set(info.record.store_id);

                    shared
                        .connections
                        .lock()
                        .map_err(|_| shutdown("poisoned maintenance registry"))?
                        .push(Arc::downgrade(&state));

                    shared.start_maintenance(info.directory.clone())?;
                    shared.wake_maintenance.notify_one();

                    return Ok(Self {
                        owner: Arc::new(ConnectionOwner {
                            state,
                            listener: None,
                        }),
                        info: Arc::new(info),
                    });
                }
                Ok(Err(ConnectionError::Open(error)))
                    if error.failure() == OpenFailure::StoreBusy =>
                {
                    retry(deadline).await?;
                }
                Ok(Err(ConnectionError::Open(error))) if error.can_recheck_initialization() => {
                    // A creator may still be publishing its initial marker. We
                    // only reread; no retry adopts an unproven guard-only store.
                    if retry(deadline).await.is_err() {
                        return Err(ConnectionError::Open(error));
                    }
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    // The previous context may have closed its last connection
                    // ahead of this queued open. No handle was published.
                    shared
                        .outcome()
                        .await
                        .map_err(|error| OpenError::caused(OpenFailure::IoError, error))?;

                    retry(deadline).await?;
                }
            }
        }
    }

    /// Snapshot access performs no I/O and remains valid after close.
    #[must_use]
    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    /// Internal host delivery registration. Does not retain this connection.
    #[must_use]
    pub fn diagnostic_receiver(&self) -> crate::diagnostics::Receiver {
        self.owner.state.context.diagnostics.receiver()
    }

    /// Admit connection-owned native work before queuing or performing it.
    /// Object implementations must also enter the commit fence before logical
    /// publication; this admission alone is not publication authorization.
    ///
    /// # Errors
    /// Refuses closing/closed connections or an exhausted accounting bound.
    pub fn admit(&self) -> Result<Operation, ConnectionError> {
        self.owner.state.admit_scoped(self.owner.listener.as_ref())
    }

    /// Fence new work and join the shared close attempt without a timeout.
    /// Dropping this future never aborts already submitted native shutdown.
    ///
    /// # Errors
    /// A failed close leaves this connection Closing and supports another
    /// attempt. Other independently opened connections remain usable.
    pub async fn close(&self) -> Result<(), ConnectionError> {
        loop {
            let action = {
                let mut activity = self
                    .owner
                    .state
                    .activity
                    .lock()
                    .map_err(|_| shutdown("poisoned connection state"))?;

                match &activity.lifecycle {
                    Lifecycle::Closed => None,
                    Lifecycle::Closing(Some(response)) => Some((None, response.clone())),
                    Lifecycle::Open | Lifecycle::Closing(None) => {
                        let (result, response) = watch::channel(None);
                        activity.lifecycle = Lifecycle::Closing(Some(response.clone()));

                        Some((
                            Some(CloseAttempt {
                                state: Arc::clone(&self.owner.state),
                                result: Some(result),
                            }),
                            response,
                        ))
                    }
                }
            };

            let Some((attempt, mut response)) = action else {
                if self.owner.state.context.sender.is_closed() {
                    self.owner.state.context.join().await?;
                }

                return Ok(());
            };

            self.owner.state.closing.send_replace(true);

            if let Some(attempt) = attempt {
                self.owner.state.wait_drained().await?;

                match self.owner.state.context.sender.reserve().await {
                    Ok(permit) => permit.send(Command::Close(attempt)),
                    Err(_) => attempt.finish(Err(shutdown("metadata worker unavailable"))),
                }
            }

            let result = loop {
                if let Some(result) = response.borrow().clone() {
                    break result;
                }

                response
                    .changed()
                    .await
                    .map_err(|_| shutdown("close result unavailable"))?;
            };

            match result {
                CloseResult::Retry => {}
                CloseResult::Finished(result) => {
                    result?;

                    if self.owner.state.context.sender.is_closed() {
                        self.owner.state.context.join().await?;
                    }

                    return Ok(());
                }
            }
        }
    }
}

impl State {
    fn admit(self: &Arc<Self>) -> Result<Operation, ConnectionError> {
        self.admit_scoped(None)
    }

    fn admit_scoped(
        self: &Arc<Self>,
        listener: Option<&Arc<listener::Scope>>,
    ) -> Result<Operation, ConnectionError> {
        let mut activity = self
            .activity
            .lock()
            .map_err(|_| shutdown("poisoned operation admission"))?;

        match activity.lifecycle {
            Lifecycle::Open => {}
            Lifecycle::Closing(_) => return Err(ConnectionError::StoreClosing),
            Lifecycle::Closed => return Err(ConnectionError::StoreClosed),
        }

        let listener_closing = listener.map(|scope| scope.closing.borrow());

        if listener_closing.as_ref().is_some_and(|closing| **closing) {
            return Err(ConnectionError::StoreClosing);
        }

        activity.operations = activity
            .operations
            .checked_add(1)
            .ok_or_else(|| OpenError::new(OpenFailure::StoreBusy))?;

        if let Some(scope) = listener {
            scope.admitted();
        }

        Ok(Operation {
            state: Arc::clone(self),
            cancellation: watch::channel(false).0,
            listener: listener.cloned(),
            progress: None,
        })
    }

    async fn wait_drained(&self) -> Result<(), ConnectionError> {
        loop {
            let mut notified = std::pin::pin!(self.drained.notified());
            notified.as_mut().enable();

            if self
                .activity
                .lock()
                .map_err(|_| shutdown("poisoned operation settlement"))?
                .operations
                == 0
            {
                return Ok(());
            }

            notified.await;
        }
    }
}

async fn retry(deadline: tokio::time::Instant) -> Result<(), ConnectionError> {
    if tokio::time::Instant::now() >= deadline {
        return Err(OpenError::new(OpenFailure::StoreBusy).into());
    }

    tokio::time::sleep_until((tokio::time::Instant::now() + RETRY_DELAY).min(deadline)).await;
    Ok(())
}

fn context(
    runtime: Id,
    identity: DirectoryIdentity,
    path: &std::path::Path,
) -> Result<Arc<Context>, ConnectionError> {
    let mut registry = CONTEXTS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| OpenError::new(OpenFailure::IoError))?;

    registry.retain(|_, context| context.strong_count() != 0);

    if let Some(context) = registry.get(&(runtime, identity)).and_then(Weak::upgrade)
        && !context.sender.is_closed()
    {
        return Ok(context);
    }

    let context = Context::start(path.to_owned(), identity)?;
    registry.insert((runtime, identity), Arc::downgrade(&context));
    Ok(context)
}

impl Context {
    fn start(path: PathBuf, identity: DirectoryIdentity) -> Result<Arc<Self>, ConnectionError> {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (finished, terminal) = watch::channel(None);

        #[cfg(test)]
        let test_hooks = Arc::new(TestHooks::default());

        #[cfg(test)]
        let worker_test_hooks = Arc::clone(&test_hooks);

        let worker = std::thread::Builder::new()
            .name("mikrio-metadata".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_worker(
                        &path,
                        identity,
                        receiver,
                        #[cfg(test)]
                        &worker_test_hooks,
                    )
                }))
                .unwrap_or_else(|_| Err(shutdown("metadata worker panicked")));

                finished.send_replace(Some(result));
            })
            .map_err(|error| OpenError::caused(OpenFailure::IoError, error))?;

        let context = Arc::new(Self {
            identity,
            sender,
            terminal,
            worker: Mutex::new(Some(worker)),
            maintenance: Mutex::new(None),
            connections: Mutex::new(Vec::new()),
            wake_maintenance: Arc::new(Notify::new()),
            diagnostics: crate::diagnostics::Diagnostics::default(),
            store_id: OnceLock::new(),
            sink: crate::diagnostics::TracingSink::capture(),
            #[cfg(test)]
            test_hooks,
        });

        Ok(context)
    }

    fn start_maintenance(self: &Arc<Self>, path: PathBuf) -> Result<(), ConnectionError> {
        let mut task = self
            .maintenance
            .lock()
            .map_err(|_| shutdown("poisoned maintenance owner"))?;

        if task.is_none() {
            *task = Some(tokio::spawn(maintenance::schedule(
                Arc::downgrade(self),
                path,
                self.terminal.clone(),
                Arc::clone(&self.wake_maintenance),
            )));
        }

        Ok(())
    }

    async fn outcome(&self) -> Result<(), ConnectionError> {
        let mut terminal = self.terminal.clone();

        loop {
            if let Some(result) = terminal.borrow().clone() {
                return result;
            }

            terminal
                .changed()
                .await
                .map_err(|_| shutdown("worker terminal result unavailable"))?;
        }
    }

    async fn join(&self) -> Result<(), ConnectionError> {
        self.outcome().await?;
        maintenance::join(self).await?;

        loop {
            let worker = {
                let mut worker = self
                    .worker
                    .lock()
                    .map_err(|_| shutdown("poisoned worker ownership"))?;

                match worker.as_ref() {
                    None => return Ok(()),
                    Some(handle) if handle.is_finished() => worker.take(),
                    Some(_) => None,
                }
            };

            if let Some(worker) = worker {
                return worker
                    .join()
                    .map_err(|_| shutdown("worker termination panicked"));
            }

            // Native cleanup is already settled. Await actual thread exit before
            // joining; never block a Tokio/event-loop thread on native work.
            tokio::task::yield_now().await;
        }
    }
}

fn run_worker(
    path: &std::path::Path,
    identity: DirectoryIdentity,
    mut receiver: mpsc::Receiver<Command>,
    #[cfg(test)] test_hooks: &TestHooks,
) -> Result<(), ConnectionError> {
    let mut store: Option<OpenedStore> = None;
    let mut connections: Vec<Weak<State>> = Vec::new();

    while let Some(command) = receiver.blocking_recv() {
        retain_live_connections(&mut connections);

        match command {
            Command::Reap => {}
            Command::Access(command) => access::run(command, store.as_mut()),
            Command::Admin(command) => admin::run(command, store.as_mut()),
            Command::Bucket(command) => buckets::run(command, store.as_mut()),
            Command::Listing(command) => listing::run(command, store.as_mut()),
            Command::BucketConfiguration(command) => {
                bucket_configuration::run(command, store.as_mut());
            }
            Command::Lifecycle(command) => lifecycle::run(command, store.as_mut()),
            Command::Multipart(command) => multipart::run(command, store.as_mut()),
            Command::Object(command) => objects::run(command, store.as_mut()),
            Command::DeleteBatch(command) => deletion::run(command, store.as_mut()),
            Command::Tags(command) => tags::run(command, store.as_mut()),
            Command::Maintenance(command) => maintenance::run(command, store.as_mut()),
            Command::Open {
                state,
                region,
                mode,
                reply,
            } => {
                #[cfg(test)]
                test_hooks
                    .open_attempts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                let result = open_connection(&mut store, path, identity, region.as_deref(), mode);

                #[cfg(test)]
                if let Ok(info) = &result {
                    let hook = test_hooks
                        .before_open_reply
                        .lock()
                        .map_err(|_| shutdown("poisoned test open hook"))?
                        .take();

                    if let Some(hook) = hook {
                        hook(info);
                    }
                }

                if result.is_ok() {
                    connections.push(Arc::downgrade(&state));
                }

                let _delivery = reply.send(result);
            }
            Command::Close(attempt) => {
                let has_others = connections
                    .iter()
                    .filter_map(Weak::upgrade)
                    .any(|state| !Arc::ptr_eq(&state, &attempt.state));

                if has_others {
                    attempt.finish(Ok(()));
                    continue;
                }

                #[cfg(test)]
                if test_hooks
                    .fail_close
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    attempt.finish(Err(shutdown("injected failure before native close")));
                    continue;
                }

                if let Err(error) = close_native(&mut store) {
                    attempt.finish(Err(error));
                    continue;
                }

                receiver.close();
                attempt.finish(Ok(()));
                return Ok(());
            }
            #[cfg(test)]
            Command::Probe {
                _state,
                work,
                reply,
            } => {
                let result = store
                    .as_mut()
                    .ok_or_else(|| io::Error::other("missing probe store"))
                    .and_then(work);

                let _delivery = reply.send(result);
            }
        }

        retain_live_connections(&mut connections);

        if store.is_some() && connections.is_empty() {
            close_native(&mut store)?;
            receiver.close();
            return Ok(());
        }
    }

    // Last handle/future disappeared: native cleanup still runs on this worker,
    // independent of whether its caller runtime is being destroyed. Drop is a
    // fallback; only explicit close can report failure to the application.
    close_native(&mut store)
}

fn retain_live_connections(connections: &mut Vec<Weak<State>>) {
    connections.retain(|connection| {
        connection.upgrade().is_some_and(|state| {
            state
                .activity
                .lock()
                .is_ok_and(|activity| !matches!(activity.lifecycle, Lifecycle::Closed))
        })
    });
}

fn open_connection(
    store: &mut Option<OpenedStore>,
    path: &std::path::Path,
    identity: DirectoryIdentity,
    region: Option<&str>,
    mode: crate::OpenMode,
) -> Result<ConnectionInfo, ConnectionError> {
    if GuardManager::open(path)
        .map_err(OpenError::from)?
        .identity()
        != identity
    {
        return Err(OpenError::new(OpenFailure::StoreCorrupt).into());
    }

    if store.is_none() {
        *store = Some(OpenedStore::open_with_mode(path, region, mode)?);
    }

    let native = store
        .as_ref()
        .ok_or_else(|| OpenError::new(OpenFailure::IoError))?;

    native.recheck(region)?;

    Ok(ConnectionInfo {
        directory: native.directory().to_owned(),
        record: native.record().clone(),
    })
}

fn close_native(store: &mut Option<OpenedStore>) -> Result<(), ConnectionError> {
    let Some(native) = store.take() else {
        return Ok(());
    };

    match native.close() {
        Ok(()) => Ok(()),
        Err((native, error)) => {
            *store = Some(*native);
            Err(ConnectionError::ShutdownFailed(Arc::new(error)))
        }
    }
}

fn shutdown(message: &'static str) -> ConnectionError {
    ConnectionError::ShutdownFailed(Arc::new(io::Error::other(message)))
}

// Mutation paths attach authoritative settlement before reaching this boundary.
// Only failures without an outcome are known to precede logical publication.
fn mutation_error(error: crate::OperationError) -> crate::OperationError {
    if error.outcome().is_some() {
        error
    } else {
        error.uncommitted()
    }
}

#[cfg(test)]
mod tests;
