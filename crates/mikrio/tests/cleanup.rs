use std::panic::{AssertUnwindSafe, catch_unwind};

use mikrio::{
    CreateBucketInput, ErrorCode, GetObjectInput, ListBucketsInput, OpenOptions, PutObjectInput,
};

#[path = "support/stores.rs"]
mod test_stores;

#[path = "../../mikrio-core/tests/support/directory.rs"]
mod test_directory;

use test_directory::TestDirectory;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy)]
enum Completion {
    Success,
    Error,
    Panic,
}

#[test]
fn cleanup_closes_all_connections_and_bodies_before_propagating_each_outcome() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    for completion in [Completion::Success, Completion::Error, Completion::Panic] {
        let mut aliases = Vec::new();
        let mut body = None;

        let outcome = catch_unwind(AssertUnwindSafe(|| {
            test_stores::run(&runtime, async |stores| {
                let path = directory.path().join(format!("{completion:?}"));
                let first = stores.open(OpenOptions::new(&path)).await?;
                let second = stores.open(OpenOptions::new(&path)).await?;
                aliases.extend([first.clone(), second]);

                first
                    .create_bucket(CreateBucketInput::builder("cleanup").build()?)
                    .await?;

                first
                    .put_object(
                        PutObjectInput::builder("cleanup", "held", b"payload".to_vec()).build()?,
                    )
                    .await?;

                body = Some(
                    first
                        .get_object(GetObjectInput::builder("cleanup", "held").build()?)
                        .await?
                        .into_body(),
                );

                match completion {
                    Completion::Success => Ok(()),
                    Completion::Error => Err("original scenario error".into()),
                    Completion::Panic => fail_scenario(),
                }
            })
        }));

        match completion {
            Completion::Success => outcome.expect("successful scenario")?,
            Completion::Error => assert_eq!(
                outcome
                    .expect("error without panic")
                    .unwrap_err()
                    .to_string(),
                "original scenario error"
            ),
            Completion::Panic => assert_eq!(
                outcome.expect_err("original panic").downcast_ref::<&str>(),
                Some(&"original scenario panic")
            ),
        }

        for store in aliases {
            let error = runtime
                .block_on(store.list_buckets(ListBucketsInput::builder().build()?))
                .unwrap_err();

            assert_eq!(error.code(), ErrorCode::StoreClosed, "{completion:?}");
        }

        let error = runtime
            .block_on(body.as_mut().expect("held body").read_chunk(1))
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::BodyClosed, "{completion:?}");
    }

    directory.finish()?;
    Ok(())
}

#[expect(
    clippy::panic,
    reason = "exercise cleanup after a scenario assertion panics"
)]
fn fail_scenario() -> TestResult {
    panic!("original scenario panic");
}
