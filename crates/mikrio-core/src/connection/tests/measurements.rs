//! Isolated allocation, payload and SQL observations, plus owned queue saturation.

use std::pin::Pin;
use std::task::Context as TaskContext;
use std::time::Instant;

use tokio::io::{AsyncRead, ReadBuf};

use super::*;

mod access;
mod sql;

const ALLOCATION_CHILD: &str = "connection::tests::measurements::persistent_allocation_child";
const SOURCE: &[u8] = &[0x71; 64 * 1024];

struct RepeatedSource(u64);

impl AsyncRead for RepeatedSource {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let length = buffer
            .remaining()
            .min(SOURCE.len())
            .min(usize::try_from(self.0).unwrap_or(usize::MAX));

        buffer.put_slice(&SOURCE[..length]);
        self.0 -= u64::try_from(length).map_err(io::Error::other)?;
        Poll::Ready(Ok(()))
    }
}

#[test]
fn native_allocation_observer_detects_live_buffers_and_bounded_large_transfers() -> TestResult {
    let _serial = crate::test_process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();
    let mut samples = Vec::new();

    for bytes in [64 * 1024, 16 * 1024 * 1024] {
        let path = directory.path().join(bytes.to_string());

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            connection.close().await
        })?;

        let mut child =
            crate::test_process::Probe::start(&path, &format!("{bytes}:1:2:0"), ALLOCATION_CHILD)?;

        let sample: serde_json::Value = serde_json::from_str(&child.observation()?)?;
        child.finish()?;
        samples.push(sample);
    }

    for sample in &samples {
        let allocation = &sample["allocations"];

        let baseline = allocation["rust_baseline"]
            .as_u64()
            .ok_or("missing baseline")?;

        let peak = allocation["rust_peak"]
            .as_u64()
            .ok_or("missing allocation peak")?;

        assert!(peak >= baseline + 64 * 1024);
        assert!(peak < baseline + 2 * 1024 * 1024);

        assert!(
            allocation["sqlite_peak"]
                .as_i64()
                .ok_or("missing SQLite peak")?
                > 0
        );
    }

    directory.finish()?;
    Ok(())
}

#[test]
fn measured_guard_waiters_leave_native_capacity_for_holder_progress() -> TestResult {
    let _serial = crate::test_process::process_scenario()?;
    let directory = TestDirectory::new()?.retain_on_failure();

    for concurrency in [1, 8, 32] {
        let path = directory.path().join(concurrency.to_string());

        runtime()?.block_on(async {
            let connection = Connection::open(path.clone(), None).await?;
            connection.close().await
        })?;

        let mut child = crate::test_process::Probe::start(
            &path,
            &format!("65536:{concurrency}:1:1"),
            ALLOCATION_CHILD,
        )?;

        let sample: serde_json::Value = serde_json::from_str(&child.observation()?)?;
        child.finish()?;

        #[cfg(unix)]
        assert!(
            sample["descriptor_limits"]["soft_after"]
                .as_u64()
                .ok_or("missing measurement descriptor budget")?
                >= 512
        );

        #[cfg(windows)]
        assert!(sample["descriptor_limits"]["rlimit_nofile"].is_null());

        assert_eq!(sample["guards"]["contended"], concurrency);
        assert_eq!(sample["guards"]["succeeded"], 2 * concurrency);

        assert!(
            sample["guards"]["retry_backoff_ns"]
                .as_u64()
                .ok_or("missing guard wait")?
                > 0
        );
    }

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned isolated process for native allocation and persistent transfer measurements"]
fn persistent_allocation_child() -> TestResult {
    let path = PathBuf::from(
        std::env::var_os("mikrio_probe_directory").ok_or("missing owned measurement directory")?,
    );

    let mode = std::env::var("mikrio_probe_mode")?;

    let values = mode
        .split(':')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()?;

    let [bytes, concurrency, rounds, hold_guards] = values.as_slice() else {
        return Err("invalid persistent measurement mode".into());
    };

    let (bytes, concurrency, rounds) = (*bytes, *concurrency, *rounds);

    let hold_guards = match hold_guards {
        0 => false,
        1 if bytes == 65_536 && rounds == 1 => true,
        _ => return Err("invalid guard contention profile".into()),
    };

    let is_supported_size = matches!(
        bytes,
        65_536 | 1_048_576 | 16_777_216 | 268_435_456 | 1_073_741_824
    );

    if !is_supported_size || !matches!(concurrency, 1 | 8 | 32) || !(1..=16).contains(&rounds) {
        return Err("persistent measurement exceeds its reviewed profiles".into());
    }

    let total_bytes = bytes
        .checked_mul(concurrency)
        .and_then(|value| value.checked_mul(rounds))
        .ok_or("persistent measurement size overflow")?;

    if total_bytes > 3 * 1024 * 1024 * 1024 {
        return Err("persistent measurement exceeds its scratch-space bound".into());
    }

    let descriptor_limits = if hold_guards {
        Some(super::descriptors::prepare()?)
    } else {
        None
    };

    runtime()?.block_on(async {
        let connection = Connection::open(path, None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(crate::object::BucketName::parse("attachments")?)
            .await?;

        let storage_before = sql::storage(&connection.info().directory)?;
        let mut guards = Vec::new();

        if hold_guards {
            let manager = GuardManager::open(&connection.info().directory)?;

            for shard in 0..=u8::MAX {
                let guard = manager
                    .try_acquire(GuardTarget::Shard(shard), GuardMode::Exclusive)?
                    .ok_or("owned contention guard unavailable")?;

                guards.push(guard);
            }
        }

        sql::install()?;
        let payload_before = crate::test_metrics::payload();
        let sample = crate::test_allocations::Sample::begin()?;
        let started = Instant::now();
        let mut tasks = Vec::new();

        for worker in 0..concurrency {
            let connection = connection.clone();

            tasks.push(tokio::spawn(async move {
                let mut samples = Vec::new();

                for _ in 0..rounds {
                    let key = format!("worker-{worker}");
                    let put_started = Instant::now();

                    let facts = connection
                        .put_object(super::objects::put(&key), RepeatedSource(bytes))
                        .await
                        .map_err(io::Error::other)?;

                    let put_ns = put_started.elapsed().as_nanos();
                    let get_started = Instant::now();

                    let (selected, mut body) = connection
                        .get_object(super::objects::read(&key))
                        .await
                        .map_err(io::Error::other)?;

                    assert_eq!(selected.length.get(), bytes);
                    assert_eq!(selected.etag, facts.etag());
                    let mut received = 0_u64;

                    while let Some(chunk) = body.read_chunk(64 * 1024).await.map_err(io::Error::other)? {
                        assert!(chunk.iter().all(|byte| *byte == 0x71));
                        received += u64::try_from(chunk.len()).map_err(io::Error::other)?;
                    }

                    assert_eq!(received, bytes);
                    samples.push((put_ns, get_started.elapsed().as_nanos()));
                }

                Ok::<_, io::Error>(samples)
            }));
        }

        if hold_guards {
            tokio::time::timeout(Duration::from_secs(2), async {
                while crate::test_metrics::guards().contended != concurrency {
                    tokio::task::yield_now().await;
                }
            })
            .await?;

            // The holder uses the same finite native pool as its waiters. A
            // blocking-acquisition regression cannot release this protection.
            crate::connection::objects::native(move || {
                drop(guards);
                Ok(())
            })
            .await?;
        }

        let mut timings = Vec::new();

        for task in tasks {
            timings.extend(task.await??);
        }

        let elapsed_ns = started.elapsed().as_nanos();
        let allocations = sample.finish()?;
        let payload_after = crate::test_metrics::payload();

        let payload_bytes = payload_after
            .0
            .checked_sub(payload_before.0)
            .ok_or("payload counter regressed")?;

        let payload_calls = payload_after
            .1
            .checked_sub(payload_before.1)
            .ok_or("payload counter regressed")?;

        assert_eq!(payload_bytes, bytes * concurrency * rounds);
        assert_eq!(payload_calls, payload_bytes / u64::try_from(SOURCE.len())?);
        let guards = crate::test_metrics::guards();
        assert_eq!(guards.acquisitions, 2 * concurrency * rounds);
        assert_eq!(guards.succeeded, guards.acquisitions);
        assert_eq!(guards.contended, if hold_guards { concurrency } else { 0 });
        let sql = sql::finish()?;
        assert_eq!(u64::try_from(sql.len())?, 2 * concurrency * rounds);
        let storage_after = sql::storage(&connection.info().directory)?;
        assert_eq!(storage_after["payload"]["staging"]["bytes"], 0);
        assert_eq!(storage_after["payload"]["objects"]["bytes"], total_bytes);

        let record = serde_json::json!({
            "scope": "isolated instrumented Rust process; requested Rust heap bytes include test futures and returned read chunks; SQLite C allocator is separate; no allocator overhead, stack or OS cache",
            "bytes": bytes, "concurrency": concurrency, "rounds": rounds, "hold_guards": u8::from(hold_guards),
            "descriptor_limits": descriptor_limits,
            "source_static_bytes": SOURCE.len(), "returned_chunk_max_bytes": 64 * 1024,
            "allocations": allocations, "elapsed_ns": elapsed_ns, "put_get_ns": timings,
            "sql": sql,
            "guards": guards,
            "guard_timing_scope": "shared shard acquisition includes native admission and file open; retry_backoff_ns counts completed cooperative retry waits only; aggregate counters from this isolated process",
            "payload_write_all_bytes": payload_bytes, "payload_write_all_calls": payload_calls,
            "storage_before": storage_before, "storage_after": storage_after,
        });

        connection.close().await?;
        let mut output = std::io::stdout().lock();
        writeln!(output, "PROBE {record}")?;
        output.flush()?;
        drop(output);
        let mut control = String::new();
        std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut control)?;

        if control != "exit\n" {
            return Err("measurement control pipe closed".into());
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "explicit qualification command retains bounded timing samples"
)]
fn bounded_metadata_queue_records_wait_separately_from_native_work() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;
        connection.pause_maintenance();
        let (started, observed) = oneshot::channel();
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let holder = connection.clone();

        let held = tokio::spawn(async move {
            holder
                .probe(move |_| {
                    let _sent = started.send(());

                    released
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(io::Error::other)?;

                    Ok(())
                })
                .await
        });

        tokio::time::timeout(Duration::from_secs(10), observed).await??;
        let mut pending = Vec::new();
        let mut closure_bytes = 0;

        for _ in 0..QUEUE_CAPACITY {
            let operation = connection.admit()?;
            let (reply, response) = oneshot::channel();
            let (timing, measured) = oneshot::channel();
            let before_reserve = Instant::now();
            let permit = connection.owner.state.context.sender.try_reserve()?;
            let admitted = before_reserve.elapsed();
            let enqueued = Instant::now();

            let work = move |store: &mut OpenedStore| {
                let _operation = operation;
                let started = Instant::now();
                // An owned real worker command; this deliberately measures
                // a cheap snapshot, not SQLite execution or payload I/O.
                let record = store.record().clone();
                assert_eq!(record.format_version, 1);
                let native = started.elapsed();
                let _sent = timing.send((started, started.duration_since(enqueued), native));
                Ok(())
            };

            closure_bytes += std::mem::size_of_val(&work);

            permit.send(Command::Probe {
                _state: Arc::clone(&connection.owner.state),
                work: Box::new(work),
                reply,
            });

            pending.push((admitted, response, measured));
        }

        assert!(matches!(
            connection.owner.state.context.sender.try_reserve(),
            Err(mpsc::error::TrySendError::Full(()))
        ));

        let released_at = Instant::now();
        release.send(())?;
        tokio::time::timeout(Duration::from_secs(10), held).await???;
        let mut samples = Vec::new();
        let queued_items = pending.len();

        for (admission, response, measured) in pending {
            tokio::time::timeout(Duration::from_secs(10), response).await???;

            let (started, queued, native) =
                tokio::time::timeout(Duration::from_secs(10), measured).await??;

            assert!(
                started >= released_at,
                "queued work ran before the owning worker was released"
            );

            samples.push(serde_json::json!({
                "admission_ns": admission.as_nanos(),
                "queue_wait_ns": queued.as_nanos(),
                "native_snapshot_ns": native.as_nanos(),
            }));
        }

        let record = serde_json::json!({
            "scope": "actual worker with a barrier-held command; snapshot execution only, not SQL or production latency",
            "capacity": QUEUE_CAPACITY,
            "queued_items": queued_items,
            "inline_command_bytes": QUEUE_CAPACITY * std::mem::size_of::<Command>(),
            "boxed_closure_bytes": closure_bytes,
            "queued_payload_bytes": 0,
            "byte_scope": "command and owned closure storage at saturation; shared Arc targets, channel internals and allocator overhead excluded",
            "samples": samples,
        });

        println!("QUEUE_MEASUREMENT {record}");
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
