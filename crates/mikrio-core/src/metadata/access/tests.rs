use std::fs::File;

use rusqlite::params;

use super::*;
use crate::access::Bootstrap;
use crate::test_directory::TestDirectory;
use crate::{BootstrapValues, EpochMillis, MetadataOpen, StoreId};

const KEY: &str = "mikrio_a00112233445566778899AABBCCDDEEFF";
const PRINCIPAL: [u8; 16] = [7; 16];
const BUCKET: [u8; 16] = [2; 16];
const ALLOW: &str =
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:*","Resource":"*"}}"#;
const DENY: &str =
    r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Action":"s3:*","Resource":"*"}}"#;
const PUBLIC: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"*"}}"#;
const BUCKET_DENY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:*","Resource":"*"}}"#;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn database(directory: &TestDirectory) -> Result<MetadataDatabase, Box<dyn std::error::Error>> {
    let path = directory.path().join("metadata.sqlite3");
    File::create_new(&path)?;
    let mut db = MetadataDatabase::open(&path, MetadataOpen::Initialize)?;

    db.initialize(&BootstrapValues::generate(
        StoreId::from_bytes([1; 16]),
        "us-east-1".into(),
        EpochMillis::new(0)?,
    )?)?;

    db.connection.execute(
        "INSERT INTO principals(principal_id,name,is_enabled,created_at_ms) VALUES(?1,'app',1,0)",
        [PRINCIPAL.as_slice()],
    )?;

    db.connection.execute(
        "INSERT INTO access_keys (access_key_id, principal_id, secret_access_key, created_at_ms)
         VALUES (?1, ?2, 'secret-canary', 0)",
        params![KEY, PRINCIPAL.as_slice()],
    )?;

    db.connection.execute(
        "INSERT INTO buckets(bucket_id,name,block_public_acls,ignore_public_acls,block_public_policy,restrict_public_buckets,created_at_ms) VALUES(?1,'photos',1,1,1,1,0)",
        [BUCKET.as_slice()],
    )?;

    Ok(db)
}

fn close(db: MetadataDatabase) -> TestResult {
    db.close().map_err(|(_, error)| error.into())
}

fn request(context: &Context) -> AccessRequest<'_> {
    AccessRequest {
        bucket: "photos",
        expected_bucket_id: Some(BUCKET),
        key: Some("album/photo.jpg"),
        action: Action::parse("s3:GetObject").unwrap(),
        context,
        expected_owner: None,
    }
}

fn identity_policy(db: &MetadataDatabase, document: Option<&str>) {
    db.connection
        .execute(
            "UPDATE principals SET policy_generation=?1,policy_json=?2",
            params![document.map(|_| [9_u8; 16].to_vec()), document],
        )
        .unwrap();
}

fn bucket_policy(db: &MetadataDatabase, document: Option<&str>) {
    db.connection
        .execute(
            "UPDATE buckets SET policy_generation=?1,policy_json=?2",
            params![document.map(|_| [8_u8; 16].to_vec()), document],
        )
        .unwrap();
}

#[test]
fn identity_or_bucket_allow_is_sufficient_but_either_explicit_deny_wins() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;
    let identity = db.resolve_application_key(KEY)?.verified();
    let context = Context::default();
    let request = request(&context);

    let owner: String =
        db.connection
            .query_row("SELECT owner_account_id FROM store_metadata", [], |row| {
                row.get(0)
            })?;

    let arn = PrincipalId::from_bytes(PRINCIPAL).arn(&owner)?;

    let scoped = format!(
        r#"{{"Version":"2012-10-17","Statement":{{"Effect":"Allow","Principal":{{"AWS":"{arn}"}},"Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/*"}}}}"#
    );

    let cases = [
        (None, None, false),
        (Some(ALLOW), None, true),
        (None, Some(scoped.as_str()), true),
        (Some(DENY), Some(scoped.as_str()), false),
        (Some(ALLOW), Some(BUCKET_DENY), false),
    ];

    for (case_index, (attached, bucket, allowed)) in cases.into_iter().enumerate() {
        identity_policy(&db, attached);
        bucket_policy(&db, bucket);
        let result = db.authorize_http(&identity, &request);

        if allowed {
            assert!(result.is_ok(), "case {case_index}: {result:?}");
        } else {
            assert!(
                matches!(result, Err(AccessError::Denied)),
                "case {case_index}"
            );
        }

        assert!(db.connection.is_autocommit(), "case {case_index}");
    }

    close(db)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn public_access_flags_remain_independent_and_do_not_disable_same_account_grants() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;
    let application = db.resolve_application_key(KEY)?.verified();
    let anonymous = HttpIdentity::anonymous();
    let context = Context::default();
    let request = request(&context);
    bucket_policy(&db, Some(PUBLIC));

    for bits in 0_u8..16 {
        let flags = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0];

        db.connection.execute(
            "UPDATE buckets SET block_public_acls=?1,ignore_public_acls=?2,block_public_policy=?3,restrict_public_buckets=?4",
            params![flags[0], flags[1], flags[2], flags[3]],
        )?;

        let result = db.authorize_http(&anonymous, &request);

        if flags[3] {
            assert!(matches!(result, Err(AccessError::Denied)), "flags={bits}");
        } else {
            assert!(result.is_ok(), "flags={bits}");
        }

        assert!(
            db.authorize_http(&application, &request).is_ok(),
            "flags={bits}"
        );
    }

    db.connection.execute(
        "UPDATE buckets SET block_public_acls=NULL,ignore_public_acls=NULL,block_public_policy=NULL,restrict_public_buckets=NULL",
        [],
    )?;

    db.authorize_http(&anonymous, &request)?;
    assert_eq!(public_access_block([None; 4])?, None);
    assert!(public_access_block([Some(0); 4])?.is_some());
    close(db)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn revoked_keys_and_stale_snapshots_cannot_authorize_new_or_publication_checks() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut first = database(&directory)?;
    identity_policy(&first, Some(ALLOW));
    let identity = first.resolve_application_key(KEY)?.verified();

    let second = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let context = Context::default();
    let request = request(&context);
    first.authorize_http(&identity, &request)?;
    first.connection.execute_batch("BEGIN")?;

    assert_eq!(
        first
            .connection
            .query_row("SELECT count(*) FROM access_keys", [], |row| {
                row.get::<_, i64>(0)
            })?,
        1
    );

    second
        .connection
        .execute("DELETE FROM access_keys WHERE access_key_id=?1", [KEY])?;

    assert_eq!(
        first
            .connection
            .query_row("SELECT count(*) FROM access_keys", [], |row| {
                row.get::<_, i64>(0)
            })?,
        1
    );

    assert!(matches!(
        first.authorize_http(&identity, &request),
        Err(AccessError::SnapshotActive)
    ));

    assert!(matches!(
        first.resolve_application_key(KEY),
        Err(AccessError::SnapshotActive)
    ));

    first.connection.execute_batch("ROLLBACK")?;

    assert!(matches!(
        first.authorize_http(&identity, &request),
        Err(AccessError::Denied)
    ));

    assert!(matches!(
        first.resolve_application_key(KEY),
        Err(AccessError::Denied)
    ));

    let tx = first
        .connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    assert!(matches!(
        authorize(&tx, &identity, &request),
        Err(AccessError::Denied)
    ));

    tx.rollback()?;
    close(first)?;
    close(second)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn bootstrap_is_listener_local_disjoint_and_has_only_narrow_policy_recovery() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;

    let first = Bootstrap::new(
        "mikrio_bfixture".into(),
        Secret::new("bootstrap-secret".into())?,
    )?;

    let second = Bootstrap::new(
        "mikrio_bfixture".into(),
        Secret::new("bootstrap-secret".into())?,
    )?;

    assert!(matches!(
        Bootstrap::new(KEY.into(), Secret::new("secret".into())?),
        Err(IdentityError::CredentialClass)
    ));

    assert!(first.resolve(KEY).is_none());

    assert!(matches!(
        db.resolve_application_key("mikrio_bfixture"),
        Err(AccessError::Denied)
    ));

    let identity = first.resolve("mikrio_bfixture").unwrap().verified();
    let context = Context::default();
    let mut request = request(&context);
    db.authorize_http(&identity, &request)?;
    bucket_policy(&db, Some(BUCKET_DENY));

    assert!(matches!(
        db.authorize_http(&identity, &request),
        Err(AccessError::Denied)
    ));

    request.key = None;

    for action in [
        "s3:GetBucketPolicy",
        "s3:PutBucketPolicy",
        "s3:DeleteBucketPolicy",
    ] {
        request.action = Action::parse(action)?;
        db.authorize_http(&identity, &request)?;
    }

    request.expected_owner = Some("not-an-account");

    assert!(matches!(
        db.authorize_http(&identity, &request),
        Err(AccessError::Invalid(PolicyError::Invalid))
    ));

    request.expected_owner = None;
    first.close();

    assert!(matches!(
        db.authorize_http(&identity, &request),
        Err(AccessError::Denied)
    ));

    db.authorize_http(
        &second.resolve("mikrio_bfixture").unwrap().verified(),
        &request,
    )?;

    close(db)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn corrupted_policy_or_bpa_fails_visibly_instead_of_becoming_an_empty_grant() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;
    let identity = db.resolve_application_key(KEY)?.verified();
    let context = Context::default();
    let request = request(&context);
    identity_policy(&db, Some("{secret-canary}"));
    let error = db.authorize_http(&identity, &request).unwrap_err();

    assert!(matches!(
        error,
        AccessError::Corrupt("invalid stored access policy")
    ));

    assert!(!format!("{error:?}").contains("secret-canary"));
    identity_policy(&db, Some(ALLOW));

    db.connection.execute_batch(
        "PRAGMA ignore_check_constraints=ON; UPDATE buckets SET restrict_public_buckets=NULL;",
    )?;

    assert!(matches!(
        db.authorize_http(&identity, &request),
        Err(AccessError::Corrupt("invalid public access block state"))
    ));

    db.connection
        .execute("UPDATE buckets SET restrict_public_buckets=1", [])?;

    db.connection
        .execute("UPDATE principals SET is_enabled=2", [])?;

    assert!(matches!(
        db.resolve_application_key(KEY),
        Err(AccessError::Corrupt("invalid principal enabled state"))
    ));

    close(db)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn prior_bucket_identity_and_changed_key_principal_are_not_rebound() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;
    identity_policy(&db, Some(ALLOW));
    let identity = db.resolve_application_key(KEY)?.verified();
    let context = Context::default();
    let request = request(&context);

    db.connection
        .execute("UPDATE buckets SET bucket_id=?1", [[3_u8; 16].as_slice()])?;

    assert!(matches!(
        db.authorize_http(&identity, &request),
        Err(AccessError::Denied)
    ));

    db.connection
        .execute("UPDATE buckets SET bucket_id=?1", [BUCKET.as_slice()])?;

    db.connection.execute(
        "INSERT INTO principals(principal_id,name,is_enabled,created_at_ms) VALUES(?1,'new-app',1,0)",
        [[8_u8; 16].as_slice()],
    )?;

    db.connection.execute(
        "UPDATE access_keys SET principal_id=?1",
        [[8_u8; 16].as_slice()],
    )?;

    assert!(matches!(
        db.authorize_http(&identity, &request),
        Err(AccessError::Denied)
    ));

    close(db)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn corrupted_signing_secret_revokes_an_already_authenticated_identity() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;
    identity_policy(&db, Some(ALLOW));
    let identity = db.resolve_application_key(KEY)?.verified();
    let context = Context::default();
    let request = request(&context);
    db.authorize_http(&identity, &request)?;

    db.connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")?;

    for (case_index, secret) in [String::new(), "x".repeat(1025)].into_iter().enumerate() {
        db.connection.execute(
            "UPDATE access_keys SET secret_access_key = ?1 WHERE access_key_id = ?2",
            params![secret, KEY],
        )?;

        assert!(
            matches!(
                db.resolve_application_key(KEY),
                Err(AccessError::Corrupt("invalid stored signing secret"))
            ),
            "secret case {case_index}"
        );

        assert!(
            matches!(
                db.authorize_http(&identity, &request),
                Err(AccessError::Corrupt("invalid stored signing secret"))
            ),
            "secret case {case_index}"
        );

        // Mutations reuse this check inside their final write transaction.
        let error = db
            .write(|transaction| {
                authorize(transaction, &identity, &request)?;
                transaction.execute("UPDATE buckets SET block_public_acls = 0", [])?;
                Ok(())
            })
            .unwrap_err();

        assert_eq!(
            error.failure(),
            crate::OperationFailure::StoreCorrupt,
            "secret case {case_index}"
        );

        assert_eq!(
            error.outcome(),
            Some(crate::MutationOutcome::NotCommitted),
            "secret case {case_index}"
        );

        assert_eq!(
            db.connection.query_row(
                "SELECT block_public_acls FROM buckets WHERE bucket_id = ?1",
                [BUCKET.as_slice()],
                |row| row.get::<_, i64>(0),
            )?,
            1
        );

        assert!(db.connection.is_autocommit());
    }

    close(db)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn orphan_credentials_remain_visible_corruption_in_both_access_reads() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;
    let identity = db.resolve_application_key(KEY)?.verified();
    let context = Context::default();
    let request = request(&context);

    db.connection
        .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM principals;")?;

    assert!(matches!(
        db.resolve_application_key(KEY),
        Err(AccessError::Corrupt("orphan signing key"))
    ));

    assert!(db.connection.is_autocommit());

    assert!(matches!(
        db.authorize_http(&identity, &request),
        Err(AccessError::Corrupt("orphan signing key"))
    ));

    assert!(db.connection.is_autocommit());
    close(db)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn supplemental_permissions_apply_to_the_destination_and_each_batch_entry() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;
    let identity = db.resolve_application_key(KEY)?.verified();
    let mut context = Context::default();
    context.insert("s3:if-match", vec!["\"etag\"".into()])?;
    let mut write = request(&context);
    write.action = Action::parse("s3:PutObject")?;
    let read = request(&context);
    let write_only = r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:PutObject","Resource":"*"}}"#;
    identity_policy(&db, Some(write_only));
    db.authorize_http(&identity, &write)?;

    assert!(matches!(
        db.authorize_http_all(&identity, &[write, read]),
        Err(AccessError::Denied)
    ));

    // A source grant never supplies a conditional destination read permission.
    let copy_scope = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::photos/source"},
        {"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::photos/album/*"}
    ]}"#;

    identity_policy(&db, Some(copy_scope));
    let mut source = request(&context);
    source.key = Some("source");
    let mut write = request(&context);
    write.action = Action::parse("s3:PutObject")?;

    assert!(matches!(
        db.authorize_http_all(&identity, &[source, write, request(&context)]),
        Err(AccessError::Denied)
    ));

    identity_policy(&db, Some(ALLOW));
    let mut write = request(&context);
    write.action = Action::parse("s3:PutObject")?;
    db.authorize_http_all(&identity, &[write, request(&context)])?;

    bucket_policy(
        &db,
        Some(
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:GetObject","Resource":"*"}}"#,
        ),
    );

    let mut write = request(&context);
    write.action = Action::parse("s3:PutObject")?;

    assert!(matches!(
        db.authorize_http_all(&identity, &[write, request(&context)]),
        Err(AccessError::Denied)
    ));

    bucket_policy(&db, None);

    // Per-item contexts cannot let a conditional entry authorize an unconditional one.
    identity_policy(
        &db,
        Some(
            r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"s3:DeleteObject","Resource":"*","Condition":{"Null":{"s3:if-match":"false"}}}}"#,
        ),
    );

    let missing = Context::default();
    let mut conditional = request(&context);
    conditional.action = Action::parse("s3:DeleteObject")?;
    let mut unconditional = request(&missing);
    unconditional.action = Action::parse("s3:DeleteObject")?;
    db.authorize_http(&identity, &conditional)?;

    assert!(matches!(
        db.authorize_http(&identity, &unconditional),
        Err(AccessError::Denied)
    ));

    assert!(matches!(
        db.authorize_http_all(&identity, &[]),
        Err(AccessError::Invalid(PolicyError::Invalid))
    ));

    assert!(db.connection.is_autocommit());
    close(db)?;
    directory.finish()?;
    Ok(())
}

#[test]
fn only_block_public_policy_controls_installation_of_supported_public_documents() {
    let public = Policy::parse(
        PUBLIC,
        PolicyKind::Bucket {
            name: "photos",
            owner_account_id: "012345678901",
        },
    )
    .unwrap();

    let private = Policy::parse(
        BUCKET_DENY,
        PolicyKind::Bucket {
            name: "photos",
            owner_account_id: "012345678901",
        },
    )
    .unwrap();

    for bits in 0_u8..16 {
        let flags = PublicAccessBlock {
            block_public_acls: bits & 1 != 0,
            ignore_public_acls: bits & 2 != 0,
            block_public_policy: bits & 4 != 0,
            restrict_public_buckets: bits & 8 != 0,
        };

        let result = flags.check_policy_install(&public);

        if bits & 4 != 0 {
            assert!(matches!(result, Err(AccessError::Denied)), "flags={bits}");
        } else {
            assert!(result.is_ok(), "flags={bits}");
        }

        assert!(flags.check_policy_install(&private).is_ok(), "flags={bits}");
    }
}

#[test]
fn owner_snapshot_survives_reopen_and_corruption_is_not_replaced() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;
    let owner = db.owner_info()?;
    close(db)?;

    let mut reopened = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    assert_eq!(reopened.owner_info()?, owner);

    reopened.connection.execute_batch(
        "PRAGMA ignore_check_constraints=ON; UPDATE store_metadata SET canonical_owner_id='bad-owner'",
    )?;

    assert!(matches!(
        reopened.owner_info(),
        Err(AccessError::Corrupt("invalid stored owner identity"))
    ));

    let retained: String = reopened.connection.query_row(
        "SELECT canonical_owner_id FROM store_metadata",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(retained, "bad-owner");
    close(reopened)?;
    directory.finish()?;
    Ok(())
}

#[test]
#[expect(
    clippy::print_stderr,
    reason = "retain scoped policy transaction timing in the native qualification log"
)]
fn bounded_adversarial_set_evaluation_settles_before_other_wal_work() -> TestResult {
    let directory = TestDirectory::new()?;
    let mut db = database(&directory)?;
    let identity = db.resolve_application_key(KEY)?.verified();

    let other = MetadataDatabase::open(
        &directory.path().join("metadata.sqlite3"),
        MetadataOpen::Existing,
    )?;

    let pattern = format!("{}b", "*a".repeat(1000));

    let mut document = serde_json::json!({"Version":"2012-10-17","Statement":{
        "Effect":"Allow", "Action":"s3:GetObject", "Resource":"*",
        "Condition":{"ForAnyValue:StringLike":{"s3:RequestObjectTagKeys":pattern}}
    }})
    .to_string();

    document.push_str(&" ".repeat(20_480 - document.len()));
    identity_policy(&db, Some(&document));
    let mut context = Context::default();
    // Deliberately use the larger evaluator envelope; real tag keys are smaller.
    context.insert("s3:RequestObjectTagKeys", vec!["a".repeat(1024); 10])?;
    let request = request(&context);

    let tx = db
        .connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let started = std::time::Instant::now();

    assert!(matches!(
        authorize(&tx, &identity, &request),
        Err(AccessError::Denied)
    ));

    let elapsed = started.elapsed();

    let error = other
        .connection
        .execute("UPDATE principals SET is_enabled=0", [])
        .unwrap_err();

    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );

    tx.rollback()?;

    other
        .connection
        .execute("UPDATE principals SET is_enabled=0", [])?;

    assert!(matches!(
        db.resolve_application_key(KEY),
        Err(AccessError::Denied)
    ));

    eprintln!(
        "access-policy-measurement document_bytes={} request_members=10 value_bytes=1024 transaction_check_us={}",
        document.len(),
        elapsed.as_micros()
    );

    close(other)?;
    close(db)?;
    directory.finish()?;
    Ok(())
}
