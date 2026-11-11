//! Offline access-state recovery through public operations and an isolated listener.

use std::io::{BufRead as _, Read as _, Write as _};
use std::path::Path;

use mikrio::{
    GetBucketPolicyInput, GetPrincipalInput, GetPrincipalPolicyInput, GetPublicAccessBlockInput,
    ListAccessKeysInput, PutBucketPolicyInput, PutObjectInput, SetPrincipalEnabledInput,
};
use mikrio_core::{GuardManager, GuardMode, GuardTarget};

use super::test_backup::{copy_directory, manifest};
use super::test_process as process;
use super::*;

const CHILD: &str = "recovery::restore_access_child";
const READ: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/kept"}}"#;
const LIVE_READ: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}"#;
const DENY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Action":"s3:GetObject","Resource":"*"}}"#;
const BUCKET_POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:DeleteObject","Resource":"arn:aws:s3:::photos/kept"}}"#;
const BOOTSTRAP_KEY: &str = "offline-restore-bootstrap";
const BOOTSTRAP_SECRET: &str = "fixture-bootstrap-must-never-enter-backup-7dc68f4e";

fn read_request(key: &mikrio::CreateAccessKeyOutput) -> Vec<u8> {
    signed(
        "GET",
        "/photos/kept",
        key.access_key().access_key_id(),
        key.secret_access_key(),
        "",
    )
}

fn assert_no_bootstrap(root: &Path, frozen: &test_backup::Manifest) -> TestResult {
    for (path, metadata) in frozen {
        let Some((length, _)) = metadata else {
            continue;
        };

        // An empty locked guard must not issue a Windows byte-range read.
        let mut file = std::fs::File::open(root.join(path))?.take(*length);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        assert_eq!(file.limit(), 0);
        assert_eq!(file.get_ref().metadata()?.len(), *length);

        for canary in [BOOTSTRAP_KEY, BOOTSTRAP_SECRET] {
            assert!(
                !bytes
                    .windows(canary.len())
                    .any(|window| window == canary.as_bytes()),
                "listener credential persisted in {}",
                path.display()
            );
        }
    }

    Ok(())
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "qualification retains the owned backup directory"
)]
fn offline_access_restore_rotates_old_keys_before_starting_an_isolated_listener() -> TestResult {
    let _serial = process::process_scenario()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/access-restore");
    std::fs::create_dir_all(&root)?;
    let directory = TestDirectory::new_in(&root)?.preserve();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }

    let source = directory.join("source");
    let backup = directory.join("backup");
    let working = directory.join("working");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let (principal, old_key, identity) = test_stores::run(&runtime, async |stores| {
        let store = stores.open(OpenOptions::new(&source)).await?;
        let peer = stores.open(OpenOptions::new(&source)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_object(
                PutObjectInput::builder("photos", "kept", b"restored bytes".to_vec()).build()?,
            )
            .await?;

        store
            .put_bucket_policy(PutBucketPolicyInput::builder("photos", BUCKET_POLICY).build()?)
            .await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("restore-app").build()?)
            .await?;

        store
            .put_principal_policy(
                PutPrincipalPolicyInput::builder(principal.principal_id(), READ).build()?,
            )
            .await?;

        let key = store
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        let server = store
            .listen(
                ListenOptions::default()
                    .credentials(Credentials::new(BOOTSTRAP_KEY, BOOTSTRAP_SECRET)?),
            )
            .await?;

        let second = peer
            .listen(
                ListenOptions::default()
                    .credentials(Credentials::new(BOOTSTRAP_KEY, BOOTSTRAP_SECRET)?),
            )
            .await?;

        status(&exchange(&server, &read_request(&key)).await?, 200);

        status(
            &exchange(
                &server,
                &signed("GET", "/photos/kept", BOOTSTRAP_KEY, BOOTSTRAP_SECRET, ""),
            )
            .await?,
            200,
        );

        let identity = store.info().store_id().to_owned();
        server.close().await?;

        store.close().await?;

        // Equal supplied key values do not share revocation across listeners.
        status(
            &exchange(
                &second,
                &signed("GET", "/photos/kept", BOOTSTRAP_KEY, BOOTSTRAP_SECRET, ""),
            )
            .await?,
            200,
        );

        peer.close().await?;
        second.close().await?;

        Ok::<_, Box<dyn std::error::Error>>((principal, key, identity))
    })?;

    // All Store users are closed; retain real exclusion throughout the byte copy.
    let manager = GuardManager::open(&source)?;

    let exclusion = manager
        .try_acquire(GuardTarget::Store, GuardMode::Exclusive)?
        .ok_or("source still in use")?;

    let frozen = manifest(&source)?;
    assert_no_bootstrap(&source, &frozen)?;
    copy_directory(&source, &backup)?;
    assert_eq!(manifest(&source)?, frozen);
    assert_eq!(manifest(&backup)?, frozen);
    copy_directory(&backup, &working)?;
    assert_eq!(manifest(&working)?, frozen);
    std::fs::write(directory.join("manifest.txt"), format!("{frozen:#?}\n"))?;

    std::fs::write(
        directory.join("expected.txt"),
        format!(
            "{}\n{}\n{}\n",
            identity,
            principal.principal_id(),
            old_key.access_key().access_key_id()
        ),
    )?;

    drop(exclusion);

    test_stores::run(&runtime, async |stores| {
        let store = stores.open(OpenOptions::new(&source)).await?;

        let peer = stores.open(OpenOptions::new(&source)).await?;

        let server = store.listen(ListenOptions::default()).await?;

        let old_read = read_request(&old_key);
        status(&exchange(&server, &old_read).await?, 200);

        peer.set_principal_enabled(
            SetPrincipalEnabledInput::builder(principal.principal_id(), false).build()?,
        )
        .await?;

        status(&exchange(&server, &old_read).await?, 403);

        peer.set_principal_enabled(
            SetPrincipalEnabledInput::builder(principal.principal_id(), true).build()?,
        )
        .await?;

        status(&exchange(&server, &old_read).await?, 200);

        peer.put_principal_policy(
            PutPrincipalPolicyInput::builder(principal.principal_id(), DENY).build()?,
        )
        .await?;

        status(&exchange(&server, &old_read).await?, 403);

        peer.put_principal_policy(
            PutPrincipalPolicyInput::builder(principal.principal_id(), LIVE_READ).build()?,
        )
        .await?;

        let live_key = peer
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        peer.delete_access_key(
            DeleteAccessKeyInput::builder(
                principal.principal_id(),
                old_key.access_key().access_key_id(),
            )
            .build()?,
        )
        .await?;

        status(&exchange(&server, &old_read).await?, 403);
        status(&exchange(&server, &read_request(&live_key)).await?, 200);

        let mut child = process::Probe::start(&directory, "restore", CHILD)?;
        let endpoint = child.observation_async().await?;

        status(&exchange_endpoint(&endpoint, &old_read).await?, 403);

        status(
            &exchange_endpoint(
                &endpoint,
                &signed("GET", "/photos/kept", BOOTSTRAP_KEY, BOOTSTRAP_SECRET, ""),
            )
            .await?,
            403,
        );

        // The child's key rotation cannot change the independently restored source.
        status(&exchange(&server, &read_request(&live_key)).await?, 200);
        status(&exchange(&server, &old_read).await?, 403);

        assert_eq!(
            peer.get_principal_policy(
                GetPrincipalPolicyInput::builder(principal.principal_id()).build()?,
            )
            .await?
            .document(),
            LIVE_READ
        );

        child.finish_async().await?;

        peer.close().await?;

        store.close().await?;

        server.close().await?;

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    assert_eq!(manifest(&backup)?, frozen);
    assert_no_bootstrap(&backup, &frozen)?;
    println!("ACCESS_RESTORE {}", directory.display());
    Ok(())
}

#[test]
#[ignore = "Owned independent restore process; rotates access before publishing its listener"]
fn restore_access_child() -> TestResult {
    let directory =
        PathBuf::from(std::env::var_os("mikrio_probe_directory").ok_or("missing directory")?);

    let expected = std::fs::read_to_string(directory.join("expected.txt"))?;
    let fields: Vec<_> = expected.lines().collect();
    assert_eq!(fields.len(), 3);
    let (stop, stopped) = tokio::sync::oneshot::channel();

    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::stdin().lock().read_line(&mut line);
        drop(stop.send(result.map(|_| line)));
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    test_stores::run(&runtime, async |stores| {
        let store = stores
            .open(OpenOptions::new(directory.join("working")))
            .await?;

        assert_eq!(store.info().store_id(), fields[0]);

        let principal = store
            .get_principal(GetPrincipalInput::builder(fields[1]).build()?)
            .await?;

        assert!(principal.is_enabled());
        assert_eq!(principal.name(), "restore-app");

        assert_eq!(
            store
                .get_principal_policy(GetPrincipalPolicyInput::builder(fields[1]).build()?)
                .await?
                .document(),
            READ
        );

        assert_eq!(
            store
                .get_bucket_policy(GetBucketPolicyInput::builder("photos").build()?)
                .await?
                .document(),
            BUCKET_POLICY
        );

        let flags = store
            .get_public_access_block(GetPublicAccessBlockInput::builder("photos").build()?)
            .await?
            .configuration();

        assert!(
            flags.block_public_acls
                && flags.ignore_public_acls
                && flags.block_public_policy
                && flags.restrict_public_buckets
        );

        let keys = store
            .list_access_keys(ListAccessKeysInput::builder(fields[1]).build()?)
            .await?;

        assert_eq!(keys.access_keys().len(), 1);
        assert_eq!(keys.access_keys()[0].access_key_id(), fields[2]);

        store
            .delete_access_key(DeleteAccessKeyInput::builder(fields[1], fields[2]).build()?)
            .await?;

        let fresh = store
            .create_access_key(CreateAccessKeyInput::builder(fields[1]).build()?)
            .await?;

        assert_ne!(fresh.access_key().access_key_id(), fields[2]);
        // Historical credentials were inspected and replaced before any bind.
        let server = store.listen(ListenOptions::default()).await?;

        let response = exchange(&server, &read_request(&fresh)).await?;

        status(&response, 200);
        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "restored bytes");

        {
            let mut output = std::io::stdout().lock();
            writeln!(output, "PROBE {}", server.endpoint())?;
            output.flush()?;
        }

        let line = tokio::time::timeout(Duration::from_secs(20), stopped).await???;
        assert_eq!(line, "exit\n");
        server.close().await?;

        store.close().await?;

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    reader
        .join()
        .map_err(|_| "restore control reader panicked")?;

    Ok(())
}
