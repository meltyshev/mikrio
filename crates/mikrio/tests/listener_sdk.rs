//! Public-boundary subprocess entries owned by `tools.test_sdk`.
#![cfg(feature = "http")]

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use mikrio::{
    ChecksumMode, CreateAccessKeyInput, CreateBucketInput, CreatePrincipalInput,
    DeleteAccessKeyInput, GetObjectInput, ListAccessKeysInput, ListenOptions, OpenOptions,
    PutObjectInput, PutPrincipalPolicyInput, Store,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const POLICY: &str = r#"{
    "Version": "2012-10-17",
    "Statement": [{
        "Effect": "Allow",
        "Action": "s3:ListAllMyBuckets",
        "Resource": "*"
    }, {
        "Effect": "Allow",
        "Action": ["s3:CreateBucket", "s3:ListBucket", "s3:GetBucketLocation",
                   "s3:DeleteBucket", "s3:PutObject", "s3:GetObject", "s3:DeleteObject",
                   "s3:PutBucketPolicy", "s3:GetBucketPolicy", "s3:DeleteBucketPolicy",
                   "s3:GetBucketPolicyStatus", "s3:PutBucketPublicAccessBlock",
                   "s3:GetBucketPublicAccessBlock", "s3:PutBucketOwnershipControls",
                   "s3:GetBucketOwnershipControls", "s3:GetBucketAcl", "s3:GetObjectAcl",
                   "s3:PutBucketCORS", "s3:GetBucketCORS", "s3:PutObjectTagging",
                   "s3:DeleteObjectTagging",
                   "s3:GetObjectTagging", "s3:GetObjectAttributes", "s3:AbortMultipartUpload",
                   "s3:ListBucketMultipartUploads", "s3:ListMultipartUploadParts",
                   "s3:GetLifecycleConfiguration", "s3:PutLifecycleConfiguration"],
        "Resource": ["arn:aws:s3:::photos", "arn:aws:s3:::photos/native-seed",
                     "arn:aws:s3:::photos/boto-*", "arn:aws:s3:::photos/node-*",
                     "arn:aws:s3:::photos/go-*",
                     "arn:aws:s3:::listener-python", "arn:aws:s3:::listener-python/*",
                     "arn:aws:s3:::listener-node", "arn:aws:s3:::listener-node/*"]
    }]
}"#;

#[test]
#[ignore = "Owned subprocess entry; tools.test_sdk listener drives real SDK clients"]
fn listener_fixture_child() -> TestResult {
    let path = PathBuf::from(std::env::var_os("mikrio_fixture_store").ok_or("missing store path")?);

    let session =
        PathBuf::from(std::env::var_os("mikrio_fixture_session").ok_or("missing session path")?);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let (stop, stopped) = tokio::sync::oneshot::channel();

    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::stdin().lock().read_line(&mut line);
        drop(stop.send(result.map(|_| line)));
    });

    runtime.block_on(async {
        let store = Store::open(OpenOptions::new(&path)).await?;
        let peer = Store::open(OpenOptions::new(&path)).await?;

        store
            .create_bucket(CreateBucketInput::builder("photos").build()?)
            .await?;

        store
            .put_object(
                PutObjectInput::builder("photos", "native-seed", b"native-to-sdk".to_vec())
                    .content_type("text/plain")
                    .metadata(vec![("title".into(), "café".into())])
                    .build()?,
            )
            .await?;

        let principal = store
            .create_principal(CreatePrincipalInput::builder("sdk-app").build()?)
            .await?;

        store
            .put_principal_policy(
                PutPrincipalPolicyInput::builder(principal.principal_id(), POLICY).build()?,
            )
            .await?;

        let key = store
            .create_access_key(CreateAccessKeyInput::builder(principal.principal_id()).build()?)
            .await?;

        {
            let mut output = std::io::stdout().lock();
            writeln!(output, "mikrio_prepared")?;
            output.flush()?;
        }

        let first = store.listen(ListenOptions::default()).await?;
        let second = peer.listen(ListenOptions::default()).await?;

        // The runner creates this private file and removes it after joining all
        // consumers. Secrets never enter the child's output or retained evidence.
        std::fs::write(
            &session,
            format!(
                "{}\n{}\n{}\n{}\n",
                second.endpoint(),
                principal.principal_id(),
                key.access_key().access_key_id(),
                key.secret_access_key(),
            ),
        )?;

        {
            let mut output = std::io::stdout().lock();
            writeln!(output, "mikrio_ready {}", first.endpoint())?;
            output.flush()?;
        }

        let line = tokio::time::timeout(Duration::from_mins(15), stopped).await???;

        if line != "stop\n" {
            return Err("fixture control channel closed unexpectedly".into());
        }

        let keys = peer
            .list_access_keys(ListAccessKeysInput::builder(principal.principal_id()).build()?)
            .await?;

        assert!(
            keys.access_keys().is_empty(),
            "the independent revoker must commit before shutdown"
        );

        first.close().await?;
        let replacement = store.listen(ListenOptions::default()).await?;
        replacement.close().await?;
        peer.close().await?;
        second.close().await?;
        verify_objects(&store).await?;
        store.close().await?;

        let reopened = Store::open(OpenOptions::new(&path)).await?;
        verify_objects(&reopened).await?;
        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    reader
        .join()
        .map_err(|_| "fixture control reader panicked")?;

    writeln!(
        std::io::stdout().lock(),
        "mikrio_observed {{\"reopened_objects\":2,\"verified_bytes\":532480,\"listeners\":2,\"revoked_keys\":1}}"
    )?;

    Ok(())
}

#[test]
#[ignore = "Owned subprocess entry; commits revocation on an independent public Store"]
fn listener_revoke_child() -> TestResult {
    let path = PathBuf::from(std::env::var_os("mikrio_fixture_store").ok_or("missing store path")?);

    let session = std::fs::read_to_string(
        std::env::var_os("mikrio_fixture_session").ok_or("missing session path")?,
    )?;

    let fields: Vec<_> = session.lines().collect();

    if fields.len() != 4 {
        return Err("invalid fixture session".into());
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let store = Store::open(OpenOptions::new(path)).await?;

        store
            .delete_access_key(DeleteAccessKeyInput::builder(fields[1], fields[2]).build()?)
            .await?;

        store.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

async fn verify_objects(store: &Store) -> TestResult {
    for (key, length, modulus) in [
        ("boto-shared", 257 * 1024, 251),
        ("node-shared", 263 * 1024, 239),
    ] {
        let object = store
            .get_object(
                GetObjectInput::builder("photos", key)
                    .checksum_mode(ChecksumMode::Enabled)
                    .build()?,
            )
            .await?;

        assert_eq!(object.content_length(), length);
        assert!(object.checksum_crc32().is_some());
        let mut body = object.into_body();
        let mut offset = 0_u64;

        while let Some(chunk) = body.read_chunk(64 * 1024).await? {
            for byte in chunk {
                assert_eq!(u64::from(byte), offset % modulus);
                offset += 1;
            }
        }

        assert_eq!(offset, length);
        body.close().await?;
    }

    Ok(())
}
