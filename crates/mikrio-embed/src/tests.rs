use super::*;

#[test]
fn bounded_admission_and_canceled_wait_release_capacity() -> Result<(), HostError> {
    let runtime = HostRuntime::start(2)?;
    let first = runtime.submit(std::future::pending::<()>())?;
    let second = runtime.submit(std::future::pending::<()>())?;
    assert!(matches!(runtime.submit(async {}), Err(HostError::Busy)));

    drop(first);
    drop(second);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);

    while runtime.admission.available_permits() != 2 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }

    let mut pending = runtime.submit(async { 42 })?;
    assert_eq!(pending.wait(Duration::from_secs(5))?, Some(42));
    Ok(())
}

#[test]
fn completed_work_releases_admission_before_abandoned_result_cleanup() -> Result<(), HostError> {
    struct Completion {
        admission: Arc<Semaphore>,
        observed: mpsc::SyncSender<bool>,
    }

    impl Drop for Completion {
        fn drop(&mut self) {
            let available = Arc::clone(&self.admission).try_acquire_owned().is_ok();
            self.observed.send(available).unwrap();
        }
    }

    let runtime = HostRuntime::start(1)?;
    let admission = Arc::clone(&runtime.admission);
    let (started, polling) = mpsc::sync_channel(1);
    let (finish, finishing) = mpsc::sync_channel(1);
    let (observed, observation) = mpsc::sync_channel(1);

    let pending = runtime.submit(async move {
        // Keep this one poll running while the caller abandons its receiver.
        // Cancellation cannot interrupt the observed poll or skip its result.
        started.send(()).unwrap();
        finishing.recv_timeout(Duration::from_secs(5)).unwrap();

        Completion {
            admission,
            observed,
        }
    })?;

    polling.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(runtime.submit(async {}), Err(HostError::Busy)));

    drop(pending);
    finish.send(()).unwrap();
    assert!(observation.recv_timeout(Duration::from_secs(5)).unwrap());

    let mut next = runtime.submit(async { 17 })?;
    assert_eq!(next.wait(Duration::from_secs(5))?, Some(17));
    Ok(())
}

#[test]
#[expect(clippy::panic, reason = "exercise containment after async suspension")]
fn panic_after_async_suspension_is_contained_at_poll_boundary() -> Result<(), HostError> {
    let runtime = HostRuntime::start(1)?;

    let mut pending = runtime.submit(async {
        tokio::task::yield_now().await;
        panic!("controlled native polling failure");
    })?;

    assert!(matches!(
        pending.wait(Duration::from_secs(5)),
        Err(HostError::Panicked)
    ));

    Ok(())
}

#[test]
fn independent_hosts_share_runtime_without_owning_each_others_shutdown() -> Result<(), HostError> {
    let first = HostRuntime::shared()?;
    let second = HostRuntime::shared()?;
    assert!(Arc::ptr_eq(&first, &second));

    drop(first);
    let mut pending = second.submit(async { 7 })?;
    assert_eq!(pending.wait(Duration::from_secs(5))?, Some(7));
    Ok(())
}

#[test]
fn admitted_upload_progresses_with_no_free_host_operation_slot() -> Result<(), HostError> {
    use tokio::io::AsyncReadExt as _;

    let runtime = HostRuntime::start(1)?;
    let (mut upload, mut reader) = upload::Upload::channel();

    let mut pending = runtime.submit(async move {
        let mut received = Vec::new();
        reader.read_to_end(&mut received).await.unwrap();
        received
    })?;

    assert!(matches!(runtime.submit(async {}), Err(HostError::Busy)));

    let request = {
        let mut demand = std::pin::pin!(upload.next());

        runtime
            .wait_transfer(demand.as_mut(), Duration::from_secs(5))?
            .unwrap()
            .unwrap()
    };

    request.write(b"full admission still progresses").unwrap();

    let mut eof = std::pin::pin!(upload.next());

    runtime
        .wait_transfer(eof.as_mut(), Duration::from_secs(5))?
        .unwrap()
        .unwrap()
        .finish();

    assert_eq!(
        pending.wait(Duration::from_secs(5))?.unwrap(),
        b"full admission still progresses"
    );

    Ok(())
}

#[test]
#[expect(clippy::panic, reason = "exercise transfer poll panic containment")]
fn transfer_wait_timeout_preserves_future_and_contains_poll_panic() -> Result<(), HostError> {
    let runtime = HostRuntime::start(1)?;
    let (release, released) = oneshot::channel();

    let mut future = std::pin::pin!(async {
        released.await.unwrap();
        17
    });

    assert_eq!(
        runtime.wait_transfer(future.as_mut(), Duration::from_millis(1))?,
        None
    );

    release.send(()).unwrap();

    assert_eq!(
        runtime.wait_transfer(future.as_mut(), Duration::from_secs(5))?,
        Some(17)
    );

    let mut panicking = std::pin::pin!(async {
        tokio::task::yield_now().await;
        panic!("controlled transfer polling failure");
    });

    assert!(matches!(
        runtime.wait_transfer(panicking.as_mut(), Duration::from_secs(5)),
        Err(HostError::Panicked)
    ));

    Ok(())
}
