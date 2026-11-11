//! Opportunistic, context-owned reclamation. Persistent rows are the work queue.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::sync::{Notify, Semaphore, oneshot, watch};

use super::objects::{contain, native};
use super::{Command as WorkerCommand, ConnectionError, Context, Operation, shutdown};
use crate::diagnostics::{Diagnostic, Work};
use crate::metadata::gc::GcPage;
use crate::metadata::lifecycle::expiration::{Candidate as ExpirationCandidate, ExpirationPage};
use crate::metadata::lifecycle::uploads::{Candidate as UploadCandidate, UploadPage};
use crate::metadata::receipts::{Receipt, ReceiptPage};
use crate::metadata::sweep::Progress;
use crate::{BlobId, Guard, GuardMode, GuardTarget, OpenedStore, OperationError, OperationFailure};

#[cfg(test)]
mod admission_tests;
mod observation;
mod retention;
mod uploads;

#[cfg(test)]
mod diagnostic_tests;

#[cfg(test)]
mod refusal_tests;

// A blocked host observer can retain one detached delivery, never storage owners.
static DELIVERY: Semaphore = Semaphore::const_new(1);

type RetirementResult = (Option<[u8; 16]>, Result<(), OperationError>);

pub(super) enum Command {
    Sample {
        after: Option<(u8, i64)>,
        reply: oneshot::Sender<Result<crate::metadata::gc::CleanupPage, OperationError>>,
        _operation: Arc<Operation>,
    },
    NextShard {
        start: u8,
        reply: oneshot::Sender<Result<Option<u8>, OperationError>>,
        _operation: Arc<Operation>,
    },
    Checkpoint {
        reply: oneshot::Sender<Result<(), OperationError>>,
        _operation: Arc<Operation>,
    },
    Advance {
        progress: Progress,
        reply: oneshot::Sender<Result<(), OperationError>>,
        _operation: Arc<Operation>,
    },
    Receipts {
        reply: oneshot::Sender<Result<Option<ReceiptPage>, OperationError>>,
        _operation: Arc<Operation>,
    },
    Expirations {
        reply: oneshot::Sender<Result<Option<ExpirationPage>, OperationError>>,
        _operation: Arc<Operation>,
    },
    ExpireObject {
        candidate: ExpirationCandidate,
        reply: oneshot::Sender<Result<(), OperationError>>,
        _operation: Arc<Operation>,
    },
    UploadExpirations {
        reply: oneshot::Sender<Result<Option<UploadPage>, OperationError>>,
        _operation: Arc<Operation>,
    },
    ExpireUpload {
        candidate: UploadCandidate,
        reply: oneshot::Sender<Result<(), OperationError>>,
        _operation: Arc<Operation>,
    },
    Prune {
        receipt: Receipt,
        reply: oneshot::Sender<Result<(), OperationError>>,
        _operation: Arc<Operation>,
        _guard: Option<Arc<Guard>>,
    },
    Failed {
        blob_id: BlobId,
        reply: oneshot::Sender<Result<(), OperationError>>,
        _operation: Arc<Operation>,
        _guard: Arc<Guard>,
    },
    Retire {
        after: Option<[u8; 16]>,
        reply: oneshot::Sender<RetirementResult>,
        _operation: Arc<Operation>,
    },
    Page {
        shard: u8,
        reply: oneshot::Sender<Result<Option<GcPage>, OperationError>>,
        _operation: Arc<Operation>,
        _guard: Arc<Guard>,
    },
    Mark {
        blob_id: BlobId,
        reply: oneshot::Sender<Result<bool, OperationError>>,
        _operation: Arc<Operation>,
        _guard: Arc<Guard>,
    },
    Finish {
        blob_id: BlobId,
        reply: oneshot::Sender<Result<(), OperationError>>,
        _operation: Arc<Operation>,
        _guard: Arc<Guard>,
    },
}

pub(super) async fn schedule(
    context: Weak<Context>,
    path: PathBuf,
    mut terminal: watch::Receiver<Option<Result<(), ConnectionError>>>,
    wake: Arc<Notify>,
) {
    let mut next_shard = 0u8;
    let mut tick = 0u8;
    let mut retired = None;
    let mut observations = observation::Sampler::default();

    // Scheduling jitter is not identity or entropy authority. Distinct process
    // seeds keep independent executors from repeatedly probing in lockstep.
    let mut jitter = std::process::id()
        .wrapping_mul(1_664_525)
        .wrapping_add(1_013_904_223);

    loop {
        // No State/Context or native guard survives the idle wait.
        if terminal.borrow().is_some() {
            return;
        }

        let mut ending = Box::pin(terminal.changed());
        let mut notified = Box::pin(wake.notified());
        jitter = jitter.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);

        let mut delay = Box::pin(tokio::time::sleep(Duration::from_millis(
            20 + u64::from(jitter % 21),
        )));

        let stopped = std::future::poll_fn(|cx| {
            if ending.as_mut().poll(cx).is_ready() {
                return std::task::Poll::Ready(true);
            }

            if notified.as_mut().poll(cx).is_ready() || delay.as_mut().poll(cx).is_ready() {
                return std::task::Poll::Ready(false);
            }

            std::task::Poll::Pending
        })
        .await;

        if stopped {
            return;
        }

        let Some(context) = context.upgrade() else {
            return;
        };

        #[cfg(test)]
        if context
            .test_hooks
            .pause_maintenance
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            continue;
        }

        let operation = context.connections.lock().ok().and_then(|mut connections| {
            connections.retain(|state| state.strong_count() != 0);

            connections
                .iter()
                .filter_map(Weak::upgrade)
                .find_map(|state| state.admit().ok())
        });

        let Some(operation) = operation else {
            continue;
        };

        let operation = Arc::new(operation);

        if tick == 0 {
            let (reply, response) = oneshot::channel();

            let queued = queue(
                &operation,
                Command::Checkpoint {
                    reply,
                    _operation: Arc::clone(&operation),
                },
            )
            .is_ok();

            if queued && let Ok(Err(error)) = response.await {
                record(&context, &error);
            }
        }

        if tick.is_multiple_of(8) {
            let (reply, response) = oneshot::channel();

            if queue(
                &operation,
                Command::Retire {
                    after: retired,
                    reply,
                    _operation: Arc::clone(&operation),
                },
            )
            .is_ok()
            {
                match response.await {
                    Ok((next, result)) => {
                        retired = next;

                        if let Err(error) = result {
                            record(&context, &error);
                        }
                    }
                    Err(_) => record(&context, &unavailable()),
                }
            }

            if let Err(error) = contain(prune_receipts(Arc::clone(&operation), path.clone())).await
            {
                record(&context, &error);
            }

            if let Err(error) = contain(retention::expire_objects(Arc::clone(&operation))).await {
                record(&context, &error);
            }

            if let Err(error) = contain(uploads::expire_uploads(Arc::clone(&operation))).await {
                record(&context, &error);
            }
        }

        tick = tick.wrapping_add(1);

        // The indexed hint skips empty shards. End that SQL snapshot before
        // taking a guard; batch selection and deletion marking recheck authority.
        let result = contain(async {
            let (reply, response) = oneshot::channel();

            queue(
                &operation,
                Command::NextShard {
                    start: next_shard,
                    reply,
                    _operation: Arc::clone(&operation),
                },
            )?;

            let Some(shard) = response.await.map_err(|_| unavailable())?? else {
                return Ok(());
            };

            // Advance even when this shard is protected or its batch fails.
            // One busy shard cannot suppress another pending shard's turn.
            next_shard = shard.wrapping_add(1);

            batch_observed(
                Arc::clone(&operation),
                path.clone(),
                shard,
                &mut observations,
            )
            .await
        })
        .await;

        if let Err(error) = result {
            record(&context, &error);
        }

        if let Err(error) = observations.observe(operation, path.clone()).await {
            record(&context, &error);
        }

        deliver(&context);
    }
}

fn record(context: &Context, error: &OperationError) {
    if let Some(store_id) = context.store_id.get() {
        context
            .diagnostics
            .record(Diagnostic::operation(*store_id, Work::Cleanup, error));
    }
}

fn deliver(context: &Context) {
    if context.diagnostics.has_host_delivery() {
        return;
    }

    let Ok(permit) = DELIVERY.try_acquire() else {
        return;
    };

    let queue = context.diagnostics.clone();
    let sink = context.sink.clone();

    if queue.stats().queued == 0 {
        return;
    }

    // Foreign observers cannot occupy the payload/runtime blocking pool. At
    // most one detached delivery thread exists; close revokes its queue without
    // joining arbitrary host code. It owns no store, body or native protection.
    let observer_failures = queue.clone();

    let delivery = std::thread::Builder::new()
        .name("mikrio-diagnostics".into())
        .spawn(move || {
            let _permit = permit;

            for _ in 0..16 {
                let Some(record) = queue.take() else {
                    break;
                };

                if sink.emit(&record, queue.stats()).is_err() {
                    queue.observer_failed();
                }
            }
        });

    match delivery {
        Ok(thread) => drop(thread),
        Err(_) => observer_failures.observer_failed(),
    }
}

#[cfg(test)]
pub(super) async fn batch(
    operation: Arc<Operation>,
    path: PathBuf,
    shard: u8,
) -> Result<(), OperationError> {
    batch_observed(operation, path, shard, &mut observation::Sampler::default()).await
}

async fn batch_observed(
    operation: Arc<Operation>,
    path: PathBuf,
    shard: u8,
    observations: &mut observation::Sampler,
) -> Result<(), OperationError> {
    let owner = Arc::clone(&operation);
    let directory = path.clone();

    let guard = native(move || {
        Ok(owner
            .guards(&directory)?
            .try_acquire(GuardTarget::Shard(shard), GuardMode::Exclusive)?)
    })
    .await?;

    let Some(guard) = guard else {
        observations.blocked();
        return Ok(());
    };

    let guard = Arc::new(guard);
    let (reply, response) = oneshot::channel();

    queue(
        &operation,
        Command::Page {
            shard,
            reply,
            _operation: Arc::clone(&operation),
            _guard: Arc::clone(&guard),
        },
    )?;

    let Some(page) = response.await.map_err(|_| unavailable())?? else {
        return Ok(());
    };

    #[cfg(test)]
    if !page.candidates.is_empty() {
        crate::protocol_fault::hit("gc", "selected")?;
    }

    let mut failure = None;
    let started = std::time::Instant::now();
    let end = page.progress.examined;
    let mut progress = page.progress;
    progress.examined = progress.cursor;
    let mut exhausted = true;

    for (row_id, candidate) in page.candidates {
        if *operation.state.closing.borrow() || started.elapsed() >= Duration::from_millis(50) {
            exhausted = false;
            break;
        }

        let blob_id = match candidate {
            Ok(blob_id) => blob_id,
            Err(error) => {
                failure.get_or_insert(error);
                progress.examined = row_id;
                continue;
            }
        };

        let result = collect(
            Arc::clone(&operation),
            Arc::clone(&guard),
            path.clone(),
            blob_id,
        )
        .await;

        if let Err(error) = result {
            // Keep advancing this bounded page even if one candidate cannot be
            // reclaimed. The pending row remains authoritative for later sweeps.
            let (reply, response) = oneshot::channel();

            if queue(
                &operation,
                Command::Failed {
                    blob_id,
                    reply,
                    _operation: Arc::clone(&operation),
                    _guard: Arc::clone(&guard),
                },
            )
            .is_ok()
            {
                let _retry_record = response.await;
            }

            failure.get_or_insert(error);
        }

        progress.examined = row_id;
    }

    if exhausted {
        progress.examined = end;
    }

    advance(Arc::clone(&operation), progress).await?;

    #[cfg(test)]
    crate::protocol_fault::hit("gc", "settled")?;

    failure.map_or(Ok(()), Err)
}

async fn advance(operation: Arc<Operation>, progress: Progress) -> Result<(), OperationError> {
    let (reply, response) = oneshot::channel();

    queue(
        &operation,
        Command::Advance {
            progress,
            reply,
            _operation: Arc::clone(&operation),
        },
    )?;

    response.await.map_err(|_| unavailable())?
}

pub(super) async fn prune_receipts(
    operation: Arc<Operation>,
    path: PathBuf,
) -> Result<(), OperationError> {
    let (reply, response) = oneshot::channel();

    queue(
        &operation,
        Command::Receipts {
            reply,
            _operation: Arc::clone(&operation),
        },
    )?;

    let Some(page) = response.await.map_err(|_| unavailable())?? else {
        return Ok(());
    };

    let started = std::time::Instant::now();
    let mut failure = None;
    let end = page.progress.examined;
    let mut progress = page.progress;
    progress.examined = progress.cursor;
    let mut exhausted = true;

    for (row_id, receipt) in page.candidates {
        if *operation.state.closing.borrow() || started.elapsed() >= Duration::from_millis(50) {
            exhausted = false;
            break;
        }

        let result = match receipt {
            Ok(receipt) => prune_one(Arc::clone(&operation), path.clone(), receipt).await,
            Err(error) => Err(error),
        };

        if let Err(error) = result {
            failure.get_or_insert(error);
        }

        progress.examined = row_id;
    }

    if exhausted {
        progress.examined = end;
    }

    advance(operation, progress).await?;

    failure.map_or(Ok(()), Err)
}

async fn prune_one(
    operation: Arc<Operation>,
    path: PathBuf,
    receipt: Receipt,
) -> Result<(), OperationError> {
    let guard = if let Some(output) = receipt.output {
        let owner = Arc::clone(&operation);

        let guard = native(move || {
            Ok(owner
                .guards(&path)?
                .try_acquire(GuardTarget::Shard(output.shard()), GuardMode::Exclusive)?)
        })
        .await?;

        let Some(guard) = guard else {
            return Ok(());
        };

        Some(Arc::new(guard))
    } else {
        None
    };

    let (reply, response) = oneshot::channel();

    queue(
        &operation,
        Command::Prune {
            receipt,
            reply,
            _operation: Arc::clone(&operation),
            _guard: guard,
        },
    )?;

    response.await.map_err(|_| unavailable())?
}

async fn collect(
    operation: Arc<Operation>,
    guard: Arc<Guard>,
    path: PathBuf,
    blob_id: BlobId,
) -> Result<(), OperationError> {
    let (reply, response) = oneshot::channel();

    queue(
        &operation,
        Command::Mark {
            blob_id,
            reply,
            _operation: Arc::clone(&operation),
            _guard: Arc::clone(&guard),
        },
    )?;

    if !response.await.map_err(|_| unavailable())?? {
        return Ok(());
    }

    let native_guard = Arc::clone(&guard);
    let native_owner = Arc::clone(&operation);

    native(move || {
        let _guard = native_guard;
        let root = native_owner.directory(&path)?;
        let shard = format!("{:02x}", blob_id.shard());

        root.open_child("staging")?
            .open_child(&shard)?
            .remove_owned(&blob_id.to_string())?;

        root.open_child("objects")?
            .open_child(&shard)?
            .remove_owned(&blob_id.to_string())?;

        Ok(())
    })
    .await?;

    let (reply, response) = oneshot::channel();

    queue(
        &operation,
        Command::Finish {
            blob_id,
            reply,
            _operation: Arc::clone(&operation),
            _guard: guard,
        },
    )?;

    response.await.map_err(|_| unavailable())?
}

fn queue(operation: &Operation, command: Command) -> Result<(), OperationError> {
    // Leave queue capacity for foreground commands; maintenance never waits on a
    // full metadata queue while holding exclusion.
    operation
        .state
        .context
        .sender
        .try_send(WorkerCommand::Maintenance(command))
        .map_err(|_| OperationError::new(OperationFailure::StoreBusy))
}

fn unavailable() -> OperationError {
    OperationError::new(OperationFailure::IoError)
}

pub(super) fn run(command: Command, store: Option<&mut OpenedStore>) {
    match command {
        Command::Sample {
            after,
            reply,
            _operation,
        } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().cleanup_sample(after));

            let _delivery = reply.send(result);
        }
        Command::NextShard {
            start,
            reply,
            _operation,
        } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().gc_next_shard(start));

            let _delivery = reply.send(result);
        }
        Command::Checkpoint { reply, _operation } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().checkpoint());

            let _delivery = reply.send(result);
        }
        Command::Advance {
            progress,
            reply,
            _operation,
        } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().advance_sweep(progress));

            let _delivery = reply.send(result);
        }
        Command::Receipts { reply, _operation } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().receipt_page());

            let _delivery = reply.send(result);
        }
        Command::Expirations { reply, _operation } => {
            let result = store.ok_or_else(unavailable).and_then(|store| {
                store
                    .database_mut()
                    .object_expiration_page(retention::now()?)
            });

            let _delivery = reply.send(result);
        }
        Command::ExpireObject {
            candidate,
            reply,
            _operation,
        } => {
            let result = store.ok_or_else(unavailable).and_then(|store| {
                store
                    .database_mut()
                    .expire_object(&candidate, retention::now()?)
            });

            let _delivery = reply.send(result);
        }
        Command::Prune {
            receipt,
            reply,
            _operation,
            _guard,
        } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().prune_receipt(&receipt));

            let _delivery = reply.send(result);
        }
        Command::UploadExpirations { reply, _operation } => {
            let result = store.ok_or_else(unavailable).and_then(|store| {
                store
                    .database_mut()
                    .upload_expiration_page(retention::now()?)
            });

            let _delivery = reply.send(result);
        }
        Command::ExpireUpload {
            candidate,
            reply,
            _operation,
        } => {
            let result = store.ok_or_else(unavailable).and_then(|store| {
                store
                    .database_mut()
                    .expire_upload(&candidate, retention::now()?)
            });

            let _delivery = reply.send(result);
        }
        Command::Failed {
            blob_id,
            reply,
            _operation,
            _guard,
        } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().gc_failed(blob_id));

            let _delivery = reply.send(result);
        }
        Command::Retire {
            after,
            reply,
            _operation,
        } => {
            let mut next = after;

            let result = store.ok_or_else(unavailable).and_then(|store| {
                next = store.database_mut().retirement_candidate(after)?;

                if let Some(bucket_id) = next {
                    store.database_mut().retire_namespace(bucket_id)?;
                }

                Ok(())
            });

            let _delivery = reply.send((next, result));
        }
        Command::Page {
            shard,
            reply,
            _operation,
            _guard,
        } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().gc_page(shard));

            let _delivery = reply.send(result);
        }
        Command::Mark {
            blob_id,
            reply,
            _operation,
            _guard,
        } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().gc_mark(blob_id));

            let _delivery = reply.send(result);
        }
        Command::Finish {
            blob_id,
            reply,
            _operation,
            _guard,
        } => {
            let result = store
                .ok_or_else(unavailable)
                .and_then(|store| store.database_mut().gc_finish(blob_id));

            let _delivery = reply.send(result);
        }
    }
}

pub(super) async fn join(context: &Context) -> Result<(), ConnectionError> {
    loop {
        let task = {
            let mut task = context
                .maintenance
                .lock()
                .map_err(|_| shutdown("poisoned maintenance owner"))?;

            match task.as_ref() {
                None => return Ok(()),
                Some(handle) if handle.is_finished() => task.take(),
                Some(_) => None,
            }
        };

        if let Some(task) = task {
            task.await
                .map_err(|_| shutdown("maintenance task panicked"))?;

            context.diagnostics.close();
            return Ok(());
        }

        tokio::task::yield_now().await;
    }
}
