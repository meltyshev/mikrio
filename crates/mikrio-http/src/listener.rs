//! Bounded socket ownership on the caller's runtime and shared core close fence.

use std::future::{Future, poll_fn};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use futures_util::FutureExt;
use mikrio_core::access::Bootstrap;
use mikrio_core::{Connection, ConnectionError, ListenerControl, ListenerLease};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use super::{ResponseOrigin, Service};

type CloseResult = Result<(), ConnectionError>;

/// Internal owned HTTP listener. Snapshots contain no reusable authority.
#[derive(Clone)]
pub struct Listener {
    owner: Arc<Owner>,
    endpoint: String,
}

struct Owner {
    control: ListenerControl,
    attempt: Arc<Mutex<watch::Receiver<Option<CloseResult>>>>,
    retry: mpsc::Sender<watch::Sender<Option<CloseResult>>>,
    finished: watch::Receiver<Option<CloseResult>>,
}

/// Completion-only observation; retains no listener, connection or close owner.
pub struct Completion(watch::Receiver<Option<CloseResult>>);

impl Completion {
    /// Wait for the existing transport/native shutdown result without starting it.
    ///
    /// # Errors
    /// Preserves the listener's shutdown result or failed task delivery.
    pub async fn wait(mut self) -> CloseResult {
        loop {
            if let Some(result) = self.0.borrow().clone() {
                return result;
            }

            self.0
                .changed()
                .await
                .map_err(|_| shutdown("listener result unavailable"))?;
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.control.close();
        // Fallback cleanup can retry a completed failure without retaining this
        // wrapper. At most one pending attempt exists behind the running one.
        let _retry = self.retry.try_send(watch::channel(None).0);
    }
}

impl Listener {
    /// Check bind and response-origin values without binding a socket.
    ///
    /// # Errors
    /// Returns invalid-argument failures for malformed IP or origin values.
    pub fn validate(host: &str, public_endpoint: Option<&str>) -> Result<(), ConnectionError> {
        parse_options(host, public_endpoint).map(|_| ())
    }

    /// Validate all options, reserve, bind once and publish a ready listener.
    /// Requires the caller's Tokio I/O and time drivers; creates no runtime.
    ///
    /// # Errors
    /// Refuses invalid IP/origin, occupied slots/ports, missing drivers and close
    /// that wins publication. Failed starts leave direct access usable.
    pub async fn bind(
        connection: &Connection,
        host: &str,
        port: u16,
        public_endpoint: Option<&str>,
        bootstrap: Arc<Bootstrap>,
    ) -> Result<Self, ConnectionError> {
        let (host, origin) = parse_options(host, public_endpoint)?;

        let lease = connection.reserve_listener()?;

        #[cfg(test)]
        tests::startup_checkpoint(tests::Startup::Reserved, None).await;

        // Tokio panics for missing drivers. Contain that setup failure before
        // task publication; the lease/socket still unwind in this owned scope.
        let socket = std::panic::AssertUnwindSafe(async {
            let _timer = tokio::time::sleep(std::time::Duration::ZERO);
            TcpListener::bind(SocketAddr::new(host, port)).await
        })
        .catch_unwind()
        .await
        .map_err(|_| invalid("listener requires Tokio I/O and time drivers"))?
        .map_err(open_error)?;

        let address = socket.local_addr().map_err(open_error)?;

        #[cfg(test)]
        tests::startup_checkpoint(tests::Startup::Bound, Some(address)).await;

        let local_host = match address.ip() {
            IpAddr::V4(host) if host.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(host) if host.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            host => host,
        };

        let endpoint = format!("http://{}", SocketAddr::new(local_host, address.port()));

        let mut service = Service::new(lease.connection().clone(), Some(Arc::clone(&bootstrap)))
            .map_err(open_error)?;

        service.public_endpoint =
            Some(origin.unwrap_or(ResponseOrigin::parse(&endpoint).map_err(open_error)?));

        #[cfg(test)]
        tests::startup_checkpoint(tests::Startup::Publishing, Some(address)).await;

        lease.ready()?;

        #[cfg(test)]
        tests::startup_checkpoint(tests::Startup::Ready, Some(address)).await;

        let (finished, response) = watch::channel(None);
        let (attempt, attempt_response) = watch::channel(None);
        let attempts = Arc::new(Mutex::new(attempt_response));
        let (retry, retries) = mpsc::channel(1);

        let owner = Arc::new(Owner {
            control: lease.control(),
            attempt: Arc::clone(&attempts),
            retry,
            finished: response,
        });

        // The result channel observes the complete owned task, including native
        // settlement. Losing the last caller triggers its cancellation scope.
        drop(tokio::spawn(
            Driver {
                lease,
                bootstrap,
                attempt,
                attempts,
                retries,
                finished,
            }
            .run(socket, Arc::new(service)),
        ));

        Ok(Self { owner, endpoint })
    }

    /// Actual bound local origin, including an OS-selected port.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Observe termination without extending the lifetime of the public wrapper.
    #[must_use]
    pub fn completion(&self) -> Completion {
        Completion(self.owner.finished.clone())
    }

    /// Observe the current close attempt, including a transport/settlement failure.
    /// Unlike final completion, a failed attempt does not wait for a retry.
    /// Canceling observation does not request shutdown or release the lease.
    ///
    /// # Errors
    /// Preserves the observed attempt's failure or unavailable result.
    pub async fn closed(&self) -> CloseResult {
        let attempt = self
            .owner
            .attempt
            .lock()
            .map_err(|_| shutdown("poisoned listener close attempt"))?
            .clone();

        Completion(attempt).wait().await
    }

    /// Fence HTTP commits, stop sockets and await native settlement. Concurrent
    /// callers join the result; dropping a wait never abandons native ownership.
    ///
    /// # Errors
    /// Reports transport-owner/native settlement failures without request values.
    pub async fn close(&self) -> CloseResult {
        self.owner.control.close();

        let attempt = {
            let mut current = self
                .owner
                .attempt
                .lock()
                .map_err(|_| shutdown("poisoned listener close attempt"))?;

            let failed = current.borrow().as_ref().is_some_and(Result::is_err);

            if failed {
                let (sender, receiver) = watch::channel(None);

                self.owner
                    .retry
                    .try_send(sender)
                    .map_err(|_| shutdown("listener retry unavailable"))?;

                *current = receiver;
            }

            current.clone()
        };

        Completion(attempt).wait().await
    }
}

struct Driver {
    lease: ListenerLease,
    bootstrap: Arc<Bootstrap>,
    attempt: watch::Sender<Option<CloseResult>>,
    attempts: Arc<Mutex<watch::Receiver<Option<CloseResult>>>>,
    retries: mpsc::Receiver<watch::Sender<Option<CloseResult>>>,
    finished: watch::Sender<Option<CloseResult>>,
}

impl Driver {
    async fn run(self, socket: TcpListener, service: Arc<Service>) {
        let lease = &self.lease;
        let mut requests = JoinSet::new();

        let result = std::panic::AssertUnwindSafe(accept(socket, service, lease, &mut requests))
            .catch_unwind()
            .await;

        lease.begin_close();
        requests.abort_all();

        let mut failure = match result {
            Ok(result) => result.err(),
            Err(_) => Some(shutdown("listener task panicked")),
        };

        while let Some(result) = requests.join_next().await {
            if result.is_err_and(|error| error.is_panic()) {
                failure = Some(shutdown("listener request task panicked"));
            }
        }

        self.settle(failure).await;
    }

    async fn settle(self, mut failure: Option<ConnectionError>) {
        let Self {
            lease,
            bootstrap,
            attempt,
            attempts,
            mut retries,
            finished,
        } = self;

        lease.begin_close();
        let mut attempt = attempt;
        let mut parent_retried = false;

        loop {
            let result = match failure.take() {
                Some(error) => Err(error),
                None => lease.finish().await,
            };

            if result.is_ok() {
                bootstrap.close();
                drop(lease);
                attempt.send_replace(Some(Ok(())));
                finished.send_replace(Some(Ok(())));
                return;
            }

            attempt.send_replace(Some(result));

            let next = {
                let mut parent = pin!(lease.connection_closing());
                let mut retry = pin!(retries.recv());

                poll_fn(|cx| {
                    if let Poll::Ready(Some(next)) = retry.as_mut().poll(cx) {
                        return Poll::Ready(Some(next));
                    }

                    if !parent_retried
                        && parent.as_mut().poll(cx).is_ready()
                        && let Ok(mut current) = attempts.lock()
                    {
                        let failed = current.borrow().as_ref().is_some_and(Result::is_err);

                        if failed {
                            parent_retried = true;
                            let (sender, receiver) = watch::channel(None);
                            *current = receiver;
                            return Poll::Ready(Some(sender));
                        }
                    }

                    retry.as_mut().poll(cx)
                })
                .await
            };

            let Some(next) = next else {
                // A failed cleanup cannot release ownership merely because
                // the foreign wrapper disappeared. Parent close still waits
                // on this lease rather than claiming unproved settlement.
                std::future::pending::<()>().await;
                return;
            };

            attempt = next;
        }
    }
}

async fn accept(
    socket: TcpListener,
    service: Arc<Service>,
    lease: &ListenerLease,
    requests: &mut JoinSet<io::Result<()>>,
) -> CloseResult {
    enum Event {
        Closing,
        Accepted(io::Result<(tokio::net::TcpStream, SocketAddr)>),
        Settled(Result<io::Result<()>, tokio::task::JoinError>),
    }

    loop {
        let event = {
            let mut closing = pin!(lease.closing());
            let mut accepting = pin!(socket.accept());

            poll_fn(|cx| {
                if closing.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(Event::Closing);
                }

                if let Poll::Ready(Some(result)) = requests.poll_join_next(cx) {
                    return Poll::Ready(Event::Settled(result));
                }

                // Limit tasks as well as active service calls. At saturation the
                // kernel backlog remains bounded; no user-space pending queue.
                if requests.len() < 64 {
                    return accepting.as_mut().poll(cx).map(Event::Accepted);
                }

                Poll::Pending
            })
            .await
        };

        match event {
            Event::Closing => return Ok(()),
            // A client transport failure ends that connection. A task panic is
            // a failed owner operation and must be observed by close callers.
            Event::Settled(Ok(_)) => {}
            Event::Settled(Err(_)) => return Err(shutdown("listener request task failed")),
            Event::Accepted(Ok((socket, _peer))) => {
                let service = Arc::clone(&service);
                requests.spawn(async move { service.serve(socket).await });
            }
            Event::Accepted(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
            Event::Accepted(Err(_)) => return Err(shutdown("listener accept failed")),
        }
    }
}

fn parse_options(
    host: &str,
    public_endpoint: Option<&str>,
) -> Result<(IpAddr, Option<ResponseOrigin>), ConnectionError> {
    let host = host
        .parse()
        .map_err(|_| invalid("invalid listener IP address"))?;

    let origin = public_endpoint
        .map(ResponseOrigin::parse)
        .transpose()
        .map_err(open_error)?;

    Ok((host, origin))
}

fn invalid(message: &'static str) -> ConnectionError {
    open_error(io::Error::new(io::ErrorKind::InvalidInput, message))
}

fn open_error(error: io::Error) -> ConnectionError {
    ConnectionError::ListenerIo(Arc::new(error))
}

fn shutdown(message: &'static str) -> ConnectionError {
    ConnectionError::ShutdownFailed(Arc::new(io::Error::other(message)))
}

#[cfg(test)]
#[path = "listener/tests.rs"]
mod tests;
