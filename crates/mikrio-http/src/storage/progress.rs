//! Bound worker stalls independently of HTTP response whitespace and socket I/O.

use futures_util::future::{BoxFuture, Either, select};
use mikrio_core::WorkProgress;
use s3s::{S3Result, s3_error};

pub(super) enum Admitted<T> {
    Complete(T),
    Running(BoxFuture<'static, S3Result<T>>),
}

/// Keep immediate validation/admission errors outside the deferred 200 response.
pub(super) async fn admit<T: Send + 'static>(
    work: impl Future<Output = S3Result<T>> + Send + 'static,
    progress: &WorkProgress,
) -> S3Result<Admitted<T>> {
    let mut admitted = progress.subscribe();
    let mut work: BoxFuture<'static, S3Result<T>> = Box::pin(monitor(work, admitted.clone()));

    match select(work.as_mut(), Box::pin(admitted.changed())).await {
        Either::Left((result, _)) => result.map(Admitted::Complete),
        Either::Right((changed, _)) => {
            if changed.is_err() {
                return work.await.map(Admitted::Complete);
            }

            Ok(Admitted::Running(work))
        }
    }
}

async fn monitor<T: Send + 'static>(
    work: impl Future<Output = S3Result<T>> + Send + 'static,
    mut observed: tokio::sync::watch::Receiver<tokio::time::Instant>,
) -> S3Result<T> {
    let mut work = Box::pin(work);

    loop {
        let deadline = *observed.borrow_and_update() + crate::progress::STALL_LIMIT;

        let event = Box::pin(select(
            Box::pin(observed.changed()),
            Box::pin(tokio::time::sleep_until(deadline)),
        ));

        match select(work.as_mut(), event).await {
            Either::Left((result, _)) => return result,
            Either::Right((Either::Left((changed, _)), _)) => {
                if changed.is_err() {
                    return match select(work, Box::pin(tokio::time::sleep_until(deadline))).await {
                        Either::Left((result, _)) => result,
                        Either::Right(_) => Err(s3_error!(RequestTimeout)),
                    };
                }
            }

            Either::Right((Either::Right(_), _)) => return Err(s3_error!(RequestTimeout)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    struct Pending(Arc<AtomicBool>);

    impl Future for Pending {
        type Output = S3Result<()>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            std::task::Poll::Pending
        }
    }

    impl Drop for Pending {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn worker_stall_drops_work_even_after_observer_closes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async {
            for closed in [false, true] {
                let (sender, receiver) = tokio::sync::watch::channel(
                    tokio::time::Instant::now() - crate::progress::STALL_LIMIT,
                );

                let sender = (!closed).then_some(sender);
                let dropped = Arc::new(AtomicBool::new(false));

                let error = monitor(Pending(Arc::clone(&dropped)), receiver)
                    .await
                    .unwrap_err();

                assert_eq!(
                    error.code().as_str(),
                    "RequestTimeout",
                    "observer_closed={closed}"
                );

                assert!(dropped.load(Ordering::SeqCst), "observer_closed={closed}");
                drop(sender);
            }
        });
    }

    #[test]
    fn completed_worker_steps_refresh_the_stall_deadline() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async {
            let (sender, receiver) = tokio::sync::watch::channel(
                tokio::time::Instant::now() - crate::progress::STALL_LIMIT,
            );

            let result = monitor(
                async move {
                    sender.send_replace(tokio::time::Instant::now());
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    Ok(7)
                },
                receiver,
            )
            .await;

            assert_eq!(result.unwrap(), 7);
        });
    }

    #[test]
    fn pre_admission_errors_and_success_are_immediate() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async {
            let progress = WorkProgress::default();

            assert!(matches!(
                admit(async { Ok(7) }, &progress).await.unwrap(),
                Admitted::Complete(7)
            ));

            let result = admit(async { Err::<(), _>(s3_error!(AccessDenied)) }, &progress).await;
            assert!(matches!(result, Err(error) if error.code().as_str() == "AccessDenied"));
        });
    }
}
