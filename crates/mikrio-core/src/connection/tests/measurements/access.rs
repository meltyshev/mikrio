//! Isolated administration/configuration allocations and actual SQL phases.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::access::{AdminPageSize, BucketAccessRequest, HttpIdentity, PrincipalId, PrincipalName};
use crate::cors::{CorsConfiguration, CorsRule};
use crate::object::BucketName;
use crate::{OperationError, PublicAccessBlock};

const PRINCIPALS: u32 = 1000;
const RULES: usize = 100;
const POLICY_BYTES: usize = 20 * 1024;
const BUCKET_POLICY: &str = r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Principal":"*","Action":"s3:PutObject","Resource":"arn:aws:s3:::measurements/*"}}"#;

struct Inputs {
    principal_id: PrincipalId,
    identity: Arc<HttpIdentity>,
    bucket: BucketAccessRequest,
    policy: String,
    cors: CorsConfiguration,
}

async fn workflow(connection: &Connection, inputs: &Inputs) -> Result<(), OperationError> {
    connection
        .put_principal_policy(inputs.principal_id, inputs.policy.clone())
        .await?;

    assert_eq!(
        connection
            .get_principal_policy(inputs.principal_id)
            .await?
            .document,
        inputs.policy
    );

    let page = connection
        .list_principals(AdminPageSize::new(PRINCIPALS).unwrap(), None)
        .await?;

    assert_eq!(page.principals.len(), usize::try_from(PRINCIPALS).unwrap());
    assert!(page.next_continuation_token.is_none());

    assert_eq!(
        connection
            .head_bucket_http(
                Arc::clone(&inputs.identity),
                inputs.bucket.bucket.clone(),
                None
            )
            .await?,
        "us-east-1"
    );

    let flags = PublicAccessBlock {
        block_public_acls: true,
        ignore_public_acls: false,
        block_public_policy: true,
        restrict_public_buckets: false,
    };

    connection
        .put_public_access_block(inputs.bucket.clone(), flags)
        .await?;

    assert_eq!(
        connection
            .get_public_access_block(inputs.bucket.clone())
            .await?,
        flags
    );

    connection
        .put_bucket_policy(inputs.bucket.clone(), BUCKET_POLICY.into())
        .await?;

    assert!(
        !connection
            .get_bucket_policy_status(inputs.bucket.clone())
            .await?
    );

    connection
        .put_bucket_cors(inputs.bucket.clone(), inputs.cors.clone())
        .await?;

    assert_eq!(
        connection.get_bucket_cors(inputs.bucket.clone()).await?,
        inputs.cors
    );

    Ok(())
}

#[test]
#[ignore = "Owned isolated administration/configuration resource measurement"]
fn access_allocation_child() -> TestResult {
    let path = PathBuf::from(
        std::env::var_os("mikrio_probe_directory").ok_or("missing owned measurement directory")?,
    );

    let mode = std::env::var("mikrio_probe_mode")?;

    let values = mode
        .split(':')
        .map(str::parse::<usize>)
        .collect::<Result<Vec<_>, _>>()?;

    let [principals, concurrency, rounds, rules] = values.as_slice() else {
        return Err("invalid access measurement mode".into());
    };

    let (concurrency, rounds) = (*concurrency, *rounds);

    if *principals != 1000 || *rules != RULES || !matches!(concurrency, 1 | 8 | 32) || rounds != 8 {
        return Err("access measurement exceeds its reviewed profiles".into());
    }

    runtime()?.block_on(measure(path, concurrency, rounds))
}

async fn measure(path: PathBuf, concurrency: usize, rounds: usize) -> TestResult {
    let seed = Connection::open(path.clone(), None).await?;
    seed.pause_maintenance();

    let bucket = BucketAccessRequest {
        bucket: BucketName::parse("measurements")?,
        expected_owner: None,
    };

    seed.create_bucket(bucket.bucket.clone()).await?;

    let principal = seed
        .create_principal(PrincipalName::new("measurement-owner".into())?)
        .await?;

    for index in 1..PRINCIPALS {
        seed.create_principal(PrincipalName::new(format!("measurement-{index:04}"))?)
            .await?;
    }

    let key = seed.create_access_key(principal.principal_id).await?;

    let identity = Arc::new(
        seed.resolve_application_key(key.access_key.access_key_id.as_str().into())
            .await?
            .verified(),
    );

    let statements = vec![
        r#"{"Effect":"Allow","Action":"s3:ListBucket","Resource":"arn:aws:s3:::measurements"}"#;
        128
    ]
    .join(",");

    let mut policy = format!(r#"{{"Version":"2012-10-17","Statement":[{statements}]}}"#);
    assert!(policy.len() < POLICY_BYTES);
    policy.push_str(&" ".repeat(POLICY_BYTES - policy.len()));

    let cors = CorsConfiguration::new(
        (0..RULES)
            .map(|index| CorsRule {
                id: Some(format!("rule-{index:03}")),
                allowed_origins: vec![format!("https://origin-{index:03}.example")],
                allowed_methods: vec!["GET".into(), "PUT".into(), "HEAD".into()],
                allowed_headers: Some(vec!["content-type".into(), "x-amz-*".into()]),
                expose_headers: Some(vec!["ETag".into()]),
                max_age_seconds: Some(3600),
            })
            .collect(),
    )?;

    let inputs = Arc::new(Inputs {
        principal_id: principal.principal_id,
        identity,
        bucket,
        policy,
        cors,
    });

    let mut connections = Vec::new();

    for _ in 0..concurrency {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connections.push(connection);
    }

    let storage_before = sql::storage(&path)?;
    sql::install()?;
    let sample = crate::test_allocations::Sample::begin()?;
    let started = Instant::now();
    let barrier = Arc::new(tokio::sync::Barrier::new(concurrency));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();

    for connection in &connections {
        let connection = connection.clone();
        let inputs = Arc::clone(&inputs);
        let barrier = Arc::clone(&barrier);
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);

        tasks.push(tokio::spawn(async move {
            let mut timings = Vec::new();
            barrier.wait().await;

            for _ in 0..rounds {
                let inflight = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(inflight, Ordering::SeqCst);
                let started = Instant::now();
                let result = workflow(&connection, &inputs).await;
                let elapsed_ns = started.elapsed().as_nanos();
                active.fetch_sub(1, Ordering::SeqCst);
                result?;
                timings.push(elapsed_ns);
            }

            Ok::<_, OperationError>(timings)
        }));
    }

    let mut timings = Vec::new();

    for task in tasks {
        timings.extend(task.await??);
    }

    let elapsed_ns = started.elapsed().as_nanos();
    let allocations = sample.finish()?;
    let sql = sql::finish()?;
    assert_eq!(timings.len(), concurrency * rounds);
    assert_eq!(sql.len(), 4 * concurrency * rounds);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    let peak_inflight = peak.load(Ordering::SeqCst);
    assert_eq!(peak_inflight, concurrency);
    let storage_after = sql::storage(&path)?;
    assert_eq!(storage_after["payload"]["objects"]["bytes"], 0);
    assert_eq!(storage_after["payload"]["staging"]["bytes"], 0);

    for connection in connections {
        connection.close().await?;
    }

    seed.close().await?;
    let reopened = Connection::open(path, None).await?;

    assert_eq!(
        reopened
            .get_principal_policy(inputs.principal_id)
            .await?
            .document,
        inputs.policy
    );

    assert_eq!(
        reopened
            .get_bucket_policy(inputs.bucket.clone())
            .await?
            .document,
        BUCKET_POLICY
    );

    assert_eq!(
        reopened.get_bucket_cors(inputs.bucket.clone()).await?,
        inputs.cors
    );

    reopened.close().await?;

    let record = serde_json::json!({
        "scope": "isolated instrumented Rust process: requested Rust heap includes fixture tasks, detached 1000-principal pages and policy/configuration inputs; SQLite C allocator and actual SQL transaction phases are separate; excludes allocator overhead, stack, HTTP, bindings, browser and OS cache",
        "principal_count": PRINCIPALS, "concurrency": concurrency, "rounds": rounds, "rule_count": RULES,
        "policy_bytes": POLICY_BYTES, "policy_statements": 128,
        "peak_inflight_workflows": peak_inflight, "completed_workflows": timings.len(),
        "operations_per_workflow": 10, "write_transactions_per_workflow": 4,
        "workflow_ns": timings, "elapsed_ns": elapsed_ns, "allocations": allocations, "sql": sql,
        "storage_before": storage_before, "storage_after": storage_after,
        "latency_scope": "complete ten-operation workflows and individual write transaction phases; not per-operation HTTP latency",
    });

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
}
