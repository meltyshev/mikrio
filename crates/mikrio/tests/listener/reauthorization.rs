use std::io::{BufRead as _, Write as _};
use std::path::Path;

use mikrio::{CreateAccessKeyOutput, PutObjectInput, SetPrincipalEnabledInput};

use super::test_process as process;
use super::*;

const CHILD: &str = "reauthorization::access_change_child";
const ALLOW: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/kept"}}"#;
const DENY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/kept"}}"#;

fn read_requests(key: &CreateAccessKeyOutput) -> Vec<Vec<u8>> {
    ["header", "query"]
        .into_iter()
        .flat_map(|mode| {
            ["GET", "HEAD"].map(|method| {
                signed_request(
                    method,
                    "/photos/kept",
                    (key.access_key().access_key_id(), key.secret_access_key()),
                    "",
                    "us-east-1",
                    &[],
                    mode,
                )
            })
        })
        .collect()
}

async fn assert_reads(servers: &[&Server], requests: &[Vec<u8>], expected: u16) -> TestResult {
    for server in servers {
        for request in requests {
            let response = exchange(server, request).await?;
            status(&response, expected);

            if expected == 403 {
                assert!(!response.to_ascii_lowercase().contains("etag:"));
                assert!(!response.contains("selected bytes"));
            }
        }
    }

    Ok(())
}

async fn change(directory: &Path, mode: &str) -> TestResult {
    let mut child = process::Probe::start(directory, mode, CHILD)?;
    assert_eq!(child.observation_async().await?, "settled");
    child.finish_async().await?;
    Ok(())
}

#[test]
fn old_signed_requests_follow_peer_disable_policy_rotation_and_reopen() -> TestResult {
    let _serial = process::process_scenario()?;
    let directory = TestDirectory::new()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let path = directory.path().join("store");
        let store = stores.open(OpenOptions::new(&path)).await?;
        let peer = stores.open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_object(
                PutObjectInput::builder("photos", "kept", b"selected bytes".to_vec()).build()?,
            )
            .await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("reader").build()?)
            .await?;

        store
            .put_principal_policy(
                PutPrincipalPolicyInput::builder(principal.principal_id(), ALLOW).build()?,
            )
            .await?;

        let old_key = store
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        // Only public identifiers cross the owned process control boundary.
        std::fs::write(
            directory.path().join("identity"),
            format!(
                "{}\n{}\n",
                principal.principal_id(),
                old_key.access_key().access_key_id()
            ),
        )?;

        let first = store.listen(ListenOptions::default()).await?;
        let second = peer.listen(ListenOptions::default()).await?;
        let servers = [&first, &second];
        let old_requests = read_requests(&old_key);
        assert_reads(&servers, &old_requests, 200).await?;

        let mut selected = store
            .get_object(GetObjectInput::builder("photos", "kept").build()?)
            .await?
            .into_body();

        assert_eq!(selected.read_chunk(8).await?.unwrap(), b"selected");

        for (mode, expected) in [
            ("disable", 403),
            ("enable", 200),
            ("deny", 403),
            ("allow", 200),
        ] {
            change(directory.path(), mode).await?;
            // Reuse the original signed bytes: no new signature or credentials
            // can explain restored access after the negative observations.
            assert_reads(&servers, &old_requests, expected).await?;
        }

        assert_eq!(selected.read_all(100).await?, b" bytes");
        selected.close().await?;
        change(directory.path(), "disable").await?;

        let replacement = peer
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        let replacement_requests = read_requests(&replacement);
        assert_reads(&servers, &replacement_requests, 403).await?;
        change(directory.path(), "revoke").await?;
        change(directory.path(), "enable").await?;
        assert_reads(&servers, &old_requests, 403).await?;
        assert_reads(&servers, &replacement_requests, 200).await?;
        first.close().await?;
        second.close().await?;
        store.close().await?;
        peer.close().await?;

        let reopened = stores.open(OpenOptions::new(&path)).await?;
        let server = reopened.listen(ListenOptions::default()).await?;
        assert_reads(&[&server], &old_requests, 403).await?;
        assert_reads(&[&server], &replacement_requests, 200).await?;
        server.close().await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
#[ignore = "Owned independent process; changes current authority for preexisting signed requests"]
fn access_change_child() -> TestResult {
    let directory =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing directory")?);

    let mode = std::env::var("mikrio_probe_mode")?;
    let identity = std::fs::read_to_string(directory.join("identity"))?;
    let fields: Vec<_> = identity.lines().collect();
    assert_eq!(fields.len(), 2);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.join("store")))
            .await?;

        match mode.as_str() {
            "disable" | "enable" => {
                store
                    .set_principal_enabled(
                        SetPrincipalEnabledInput::builder(fields[0], mode == "enable").build()?,
                    )
                    .await?;
            }
            "deny" | "allow" => {
                store
                    .put_principal_policy(
                        PutPrincipalPolicyInput::builder(
                            fields[0],
                            if mode == "allow" { ALLOW } else { DENY },
                        )
                        .build()?,
                    )
                    .await?;
            }
            "revoke" => {
                store
                    .delete_access_key(DeleteAccessKeyInput::builder(fields[0], fields[1]).build()?)
                    .await?;
            }
            _ => return Err("unknown access change".into()),
        }

        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    let mut output = std::io::stdout().lock();
    writeln!(output, "PROBE settled")?;
    output.flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(())
}
