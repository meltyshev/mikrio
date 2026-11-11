//! Executor cancellation tests with operation-owned file guards and permits.

use std::io;
use std::io::Write;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::test_directory::TestDirectory;
use crate::{GuardManager, GuardMode, GuardTarget};

#[test]
fn canceled_caller_and_waiter_do_not_release_running_native_work()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let manager = GuardManager::open(directory.path())?;

    let guard = manager
        .try_acquire(GuardTarget::Store, GuardMode::Shared)?
        .unwrap();

    let mut file = std::fs::File::create_new(directory.path().join("owned-work"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()?;

    runtime.block_on(async {
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&permits).acquire_owned().await?;
        let (started, observed_start) = tokio::sync::oneshot::channel();
        let (reply, response) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::sync_channel(1);

        // The resource owner, not the response-waiting caller, owns this handle.
        // The probe always observes native completion, including after abort.
        let native = tokio::task::spawn_blocking(move || -> io::Result<()> {
            let _guard = guard;
            let _permit = permit;
            file.write_all(b"before cancellation\n")?;
            crate::sync_file(&file)?;
            let _observation = started.send(());

            released
                .recv_timeout(Duration::from_secs(10))
                .map_err(io::Error::other)?;

            file.write_all(b"after cancellation\n")?;
            crate::sync_file(&file)?;
            let _delivery = reply.send(());
            Ok(())
        });

        // A timeout here fails the test; it is not evidence that work stopped.
        tokio::time::timeout(Duration::from_secs(10), observed_start).await??;
        let caller = tokio::spawn(response);
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_none()
        );

        assert_eq!(permits.available_permits(), 0);

        let (attempted, observed_attempt) = tokio::sync::oneshot::channel();
        let waiting_permits = Arc::clone(&permits);

        let waiter = tokio::spawn(async move {
            let _observation = attempted.send(());
            waiting_permits.acquire_owned().await
        });

        observed_attempt.await?;
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert_eq!(permits.available_permits(), 0);

        release.send(())?;
        tokio::time::timeout(Duration::from_secs(10), native).await???;
        assert_eq!(permits.available_permits(), 1);

        assert!(
            manager
                .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
                .is_some()
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    assert_eq!(
        std::fs::read(directory.path().join("owned-work"))?,
        b"before cancellation\nafter cancellation\n"
    );

    directory.finish()?;
    Ok(())
}
