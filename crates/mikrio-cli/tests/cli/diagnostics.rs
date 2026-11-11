use mikrio::{CreateBucketInput, DeleteObjectInput, GetObjectInput, OpenOptions, PutObjectInput};

use super::*;

#[test]
fn idle_maintenance_failures_are_redacted_on_stderr_and_shutdown_settles() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    for json_output in [false, true] {
        let store_name = if json_output {
            "json-store"
        } else {
            "human-store"
        };
        let path = directory.path().join(store_name);

        test_stores::run(&runtime, async |stores| {
            let store = stores.open(OpenOptions::new(&path)).await?;

            store
                .create_bucket(CreateBucketInput::builder("uploads").build()?)
                .await?;

            store
                .put_object(
                    PutObjectInput::builder(
                        "uploads",
                        "private-key-canary",
                        b"private-payload-canary".to_vec(),
                    )
                    .build()?,
                )
                .await?;

            let mut body = store
                .get_object(GetObjectInput::builder("uploads", "private-key-canary").build()?)
                .await?
                .into_body();

            let mut payloads = Vec::new();

            for shard in fs::read_dir(path.join("objects"))? {
                for payload in fs::read_dir(shard?.path())? {
                    payloads.push(payload?.path());
                }
            }

            assert_eq!(payloads.len(), 1);
            let payload_path = &payloads[0];
            let retained = payload_path.with_file_name("private-path-canary");

            store
                .delete_object(DeleteObjectInput::builder("uploads", "private-key-canary").build()?)
                .await?;

            // The held reader fences physical collection. Replace its retired
            // payload with an unexpected directory to exercise ordinary retry.
            fs::rename(payload_path, &retained)?;
            fs::create_dir(payload_path)?;
            body.close().await?;
            store.close().await?;

            let mut arguments = vec!["serve", store_name];

            if json_output {
                arguments.push("--json");
            }

            let mut invocation = command(directory.path(), &arguments);

            invocation
                .env("mikrio_access_key_id", "private-owner-canary")
                .env("mikrio_secret_access_key", "private-secret-canary");

            let child = Process::spawn(invocation)?;

            if json_output {
                child.ready()?;
            } else {
                assert!(child.diagnostic()?.starts_with("Endpoint: "));
                assert!(child.diagnostic()?.starts_with("Region: "));
            }

            let warning = child.diagnostic()?;
            assert!(!warning.contains("canary"));
            assert!(!warning.contains(path.to_string_lossy().as_ref()));

            if json_output {
                let record: Value = serde_json::from_str(&warning)?;
                assert_eq!(record.as_object().ok_or("diagnostic object")?.len(), 4);
                assert_eq!(record["schemaVersion"], 1);
                assert_eq!(record["event"], "diagnostic");
                assert_eq!(record["level"], "warn");

                assert!(
                    record["message"]
                        .as_str()
                        .ok_or("diagnostic message")?
                        .contains("work=cleanup")
                );
            } else {
                assert!(warning.starts_with("mikrio: warn: storage work needs attention:"));
            }

            fs::remove_dir(payload_path)?;
            fs::rename(&retained, payload_path)?;
            child.signal("-TERM")?;
            let output = child.finish()?;
            assert_eq!(output.status.code(), Some(0));
            assert!(!String::from_utf8(output.stderr)?.contains("canary"));

            if !json_output {
                assert_eq!(output.stdout, Vec::<u8>::new());
            }

            success(&run(
                directory.path(),
                &["principal", "list", store_name, "--json"],
            )?)?;

            Ok(())
        })?;
    }

    directory.finish()?;
    Ok(())
}
