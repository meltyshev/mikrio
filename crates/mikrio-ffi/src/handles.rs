//! Safe ownership of native operations behind foreign wrappers.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mikrio::{MutationOutcome, PutObjectInput, PutObjectOutput, Store};
use mikrio_embed::upload::Upload;
use mikrio_embed::{CloseGate, HostRuntime, Pending, ProcessBound};

use crate::error::Error;

const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) struct Connection {
    pub runtime: Arc<HostRuntime>,
    pub store: Store,
    pub close: CloseGate,
    pub diagnostics: mikrio::embedding::Diagnostics,
    pub registered: Arc<AtomicBool>,
    // Close publishes revocation with Release; the observer checks it with
    // Acquire before taking another event. An in-flight callback drains separately.
    pub diagnostic_closed: Arc<AtomicBool>,
}

pub(crate) struct StoreHandle(pub ProcessBound<Connection>);

pub(crate) struct ServerHandle(pub ProcessBound<Listener>);

pub(crate) struct Listener {
    pub runtime: Arc<HostRuntime>,
    pub server: mikrio::Server,
    pub close: CloseGate,
}

impl Listener {
    pub(crate) fn close(&self) -> Result<(), Error> {
        let server = self.server.clone();

        let wait = self
            .close
            .begin(&self.runtime, async move { server.close().await })?;

        wait_close(&wait)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let server = self.server.clone();

        let _cleanup = self
            .close
            .begin(&self.runtime, async move { server.close().await });
    }
}

fn wait_close(wait: &mikrio_embed::CloseWait) -> Result<(), Error> {
    loop {
        let Some(outcome) = wait.wait(POLL_INTERVAL)? else {
            continue;
        };

        return match outcome.as_ref() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(Error::from(error)),
            Err(error) => Err(Error::from(error)),
        };
    }
}

impl Connection {
    pub(crate) fn close(&self) -> Result<(), Error> {
        self.diagnostic_closed.store(true, Ordering::Release);
        let store = self.store.clone();

        let wait = self
            .close
            .begin(&self.runtime, async move { store.close().await })?;

        wait_close(&wait)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.diagnostic_closed.store(true, Ordering::Release);

        // The close task retains the runtime and connection through native
        // settlement. Foreign release never waits for or frees active I/O.
        let store = self.store.clone();

        let _cleanup = self
            .close
            .begin(&self.runtime, async move { store.close().await });
    }
}

pub(crate) fn wait<T>(mut pending: Pending<T>) -> Result<T, Error> {
    loop {
        if let Some(result) = pending.wait(POLL_INTERVAL)? {
            return Ok(result);
        }
    }
}

pub(crate) fn wait_mutation<T>(pending: Pending<T>) -> Result<T, Error> {
    // Once admitted, a lost host result cannot prove whether publication ran.
    // Preserve the inner native result and its authoritative mutation outcome.
    wait(pending).map_err(|error| error.mutation(MutationOutcome::Unknown))
}

pub(crate) fn transfer<F>(runtime: &HostRuntime, future: F) -> Result<F::Output, Error>
where
    F: Future + Send,
    F::Output: Send,
{
    let mut future = std::pin::pin!(future);

    loop {
        if let Some(result) = runtime.wait_transfer(future.as_mut(), POLL_INTERVAL)? {
            return Ok(result);
        }
    }
}

pub(crate) struct WriterHandle(pub ProcessBound<Writer>);

enum Written {
    Object(PutObjectOutput),
    Part(mikrio::UploadPartOutput),
}

pub(crate) struct Writer {
    is_part: bool,
    runtime: Arc<HostRuntime>,
    upload: Option<Upload>,
    pending: Option<Pending<Result<Written, mikrio::StorageError>>>,
    failure: Option<Error>,
    outcome: Option<MutationOutcome>,
}

impl Writer {
    pub(crate) fn begin(
        owner: &Connection,
        upload: Upload,
        input: PutObjectInput,
    ) -> Result<Self, Error> {
        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.put_object(input).await.map(Written::Object) })
            .map_err(|error| Error::from(error).mutation(MutationOutcome::NotCommitted))?;

        Ok(Self {
            is_part: false,
            runtime: Arc::clone(&owner.runtime),
            upload: Some(upload),
            pending: Some(pending),
            failure: None,
            outcome: None,
        })
    }

    pub(crate) fn begin_part(
        owner: &Connection,
        upload: Upload,
        input: mikrio::UploadPartInput,
    ) -> Result<Self, Error> {
        let store = owner.store.clone();

        let pending = owner
            .runtime
            .submit(async move { store.upload_part(input).await.map(Written::Part) })
            .map_err(|error| Error::from(error).mutation(MutationOutcome::NotCommitted))?;

        Ok(Self {
            is_part: true,
            runtime: Arc::clone(&owner.runtime),
            upload: Some(upload),
            pending: Some(pending),
            failure: None,
            outcome: None,
        })
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.upload.is_none() {
            return Err(self.ended());
        }

        for chunk in bytes.chunks(mikrio_embed::chunks::CHUNK_BYTES) {
            let upload = self
                .upload
                .as_mut()
                .ok_or_else(|| Error::new("BodyClosed", "upload writer is terminal"))?;

            let request = match transfer(&self.runtime, upload.next()) {
                Ok(request) => request,
                Err(error) => return Err(self.transfer_failed(error)),
            };

            let Some(request) = request else {
                return self.result().map(|_| ());
            };

            if request.write(chunk).is_err() {
                return self.result().map(|_| ());
            }
        }

        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<PutObjectOutput, Error> {
        if self.is_part {
            return Err(Error::invalid("part writer requires writer_finish_part")
                .mutation(self.outcome.unwrap_or(MutationOutcome::NotCommitted)));
        }

        match self.finish_value()? {
            Written::Object(value) => Ok(value),
            Written::Part(_) => Err(Error::new("IoError", "writer result kind mismatch")
                .mutation(MutationOutcome::Committed)),
        }
    }

    pub(crate) fn finish_part(&mut self) -> Result<mikrio::UploadPartOutput, Error> {
        if !self.is_part {
            return Err(Error::invalid("object writer requires writer_finish")
                .mutation(self.outcome.unwrap_or(MutationOutcome::NotCommitted)));
        }

        match self.finish_value()? {
            Written::Part(value) => Ok(value),
            Written::Object(_) => Err(Error::new("IoError", "writer result kind mismatch")
                .mutation(MutationOutcome::Committed)),
        }
    }

    pub(crate) fn abort(&mut self) -> Result<(), Error> {
        if self.pending.is_none() {
            return Err(self.ended());
        }

        // Closing demand fails the source. Await the actual operation result;
        // release alone drops Pending and requests cancellation without EOF.
        self.upload.take();

        match self.result() {
            Err(error) if error.outcome == Some(MutationOutcome::NotCommitted) => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(
                Error::new("IoError", "aborted upload unexpectedly committed")
                    .mutation(MutationOutcome::Committed),
            ),
        }
    }

    fn finish_value(&mut self) -> Result<Written, Error> {
        let Some(upload) = self.upload.as_mut() else {
            return Err(self.ended());
        };

        let request = match transfer(&self.runtime, upload.next()) {
            Ok(request) => request,
            Err(error) => return Err(self.transfer_failed(error)),
        };

        if let Some(request) = request {
            request.finish();
        }

        self.result()
    }

    fn ended(&self) -> Error {
        self.failure.clone().unwrap_or_else(|| {
            let mut error = Error::new("BodyClosed", "upload writer is terminal");
            error.outcome = self.outcome;

            error
        })
    }

    fn result(&mut self) -> Result<Written, Error> {
        self.upload.take();

        let Some(pending) = self.pending.take() else {
            return Err(self.ended());
        };

        let result = wait(pending)
            .map_err(|error| error.mutation(MutationOutcome::Unknown))
            .and_then(|result| result.map_err(Error::from));

        if let Err(error) = &result {
            self.failure = Some(error.clone());
            self.outcome = error.outcome;
        } else {
            self.outcome = Some(MutationOutcome::Committed);
        }

        result
    }

    fn transfer_failed(&mut self, mut primary: Error) -> Error {
        // A failed waiter cannot make this cursor reusable. Revoke its producer
        // and join the actual PUT before assigning mutation context.
        self.upload.take();

        match self.result() {
            Ok(_) => primary.outcome = Some(MutationOutcome::Committed),
            Err(settlement) => {
                primary.outcome = settlement.outcome.or(Some(MutationOutcome::Unknown));
                primary.cause = Some(settlement.message);
            }
        }

        self.failure = Some(primary.clone());
        self.outcome = primary.outcome;

        primary
    }
}

pub(crate) struct BodyHandle(pub ProcessBound<Body>);

pub(crate) struct Body {
    pub runtime: Arc<HostRuntime>,
    pub body: mikrio_embed::body::Body,
}

impl Drop for Body {
    fn drop(&mut self) {
        let body = self.body.clone();
        let close = CloseGate::default();

        // Keep the executor alive through cleanup even if this was its last
        // foreign wrapper. No per-chunk task or ordinary admission is needed.
        let _cleanup = close.begin(&self.runtime, async move { body.close().await });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_transfer_joins_real_put_before_making_the_writer_terminal() -> Result<(), String> {
        crate::test_store::run("transfer", |owner| {
            let store = owner.store.clone();

            wait(
                owner
                    .runtime
                    .submit(async move {
                        store
                            .create_bucket(
                                mikrio::CreateBucketInput::builder("attachments").build()?,
                            )
                            .await
                    })
                    .expect("bucket admission"),
            )
            .expect("bucket delivery")
            .expect("bucket creation");

            let (upload, source) = Upload::channel();

            let input = PutObjectInput::builder("attachments", "owned", source)
                .build()
                .expect("valid upload");

            let mut writer = Writer::begin(owner, upload, input).expect("writer admission");

            writer.write(b"prefix").expect("real source consumption");

            let failure =
                writer.transfer_failed(Error::new("IoError", "controlled transfer failure"));

            assert_eq!(failure.outcome, Some(MutationOutcome::NotCommitted));
            assert!(failure.cause.is_some());
            assert!(writer.pending.is_none() && writer.upload.is_none());

            assert_eq!(
                writer.write(b"late").expect_err("terminal writer").message,
                failure.message
            );

            let store = owner.store.clone();

            let selected = wait(
                owner
                    .runtime
                    .submit(async move {
                        store
                            .head_object(
                                mikrio::HeadObjectInput::builder("attachments", "owned").build()?,
                            )
                            .await
                    })
                    .expect("read admission"),
            )
            .expect("read delivery");

            assert_eq!(
                selected.expect_err("failed producer cannot publish").code(),
                mikrio::ErrorCode::NoSuchKey
            );
        })
    }
}
