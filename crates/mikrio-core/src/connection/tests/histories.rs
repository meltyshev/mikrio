//! Observable histories over the real engine, with a bounded independent oracle.

use std::fmt::Write as _;
use std::fs::File;
use std::sync::Mutex;

use md5::{Digest, Md5};
use sha2::Sha256;
use tokio::sync::Barrier;
use tokio::task::JoinSet;

use super::objects::{put, read};
use super::*;
use crate::body::ReadBody;
use crate::object::{BucketName, ObjectTags, StoredObject, UserMetadata, WriteConditions};
use crate::{MutationOutcome, OperationError, OperationFailure};

mod model;
mod outcomes;
use model::{Action, Condition, Entry, Model, Object, Response, reconcile};

const KEY: &str = "literal/%2F/雪";
const SEEDS: [u64; 4] = [1, 0x5eed, 0xdead_beef, 0x1234_5678_9abc_def0];

struct History {
    clock: AtomicUsize,
    log: Mutex<File>,
}

impl History {
    fn begin(&self, connection: usize, action: Action) -> usize {
        let at = self.clock.fetch_add(1, Ordering::SeqCst);

        self.event(&serde_json::json!({
            "event": "invoke", "at": at, "connection": connection, "action": action,
        }));

        at
    }

    fn reply(
        &self,
        connection: usize,
        action: Action,
        invoked: usize,
        response: Response,
    ) -> Entry {
        let entry = Entry {
            connection,
            action,
            invoked,
            responded: self.clock.fetch_add(1, Ordering::SeqCst),
            response,
        };

        self.event(&serde_json::json!({"event": "response", "entry": entry}));

        entry
    }

    fn event(&self, event: &serde_json::Value) {
        let mut log = self.log.lock().unwrap();
        serde_json::to_writer(&mut *log, event).unwrap();
        writeln!(log).unwrap();
        log.flush().unwrap();
    }
}

struct Recorded {
    entry: Entry,
    body: Option<(Object, ReadBody)>,
}

fn payload(value: u8) -> Vec<u8> {
    // Distinct lengths and content, all exceeding the engine's retained chunk.
    (0..131_101 + usize::from(value))
        .map(|index| u8::try_from(index % 251).unwrap() ^ value)
        .collect()
}

fn etag(value: u8) -> String {
    format!("\"{}\"", hex(&Md5::digest(payload(value))))
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);

    for byte in bytes {
        write!(encoded, "{byte:02x}").unwrap();
    }

    encoded
}

fn selected(object: &StoredObject) -> Object {
    let value = object.metadata.entries()["version"].parse::<u8>().unwrap();
    assert!((1..=3).contains(&value));
    assert_eq!(object.metadata.entries().len(), 1);

    assert_eq!(
        object.length.get(),
        u64::try_from(payload(value).len()).unwrap()
    );

    assert_eq!(object.etag, etag(value));

    let tag = object
        .tags
        .entries()
        .get("status")
        .map(|value| value.parse::<u8>().unwrap());

    assert_eq!(object.tags.entries().len(), usize::from(tag.is_some()));
    assert_eq!(object.tag_count, Some(u32::from(tag.is_some())));
    Object { value, tag }
}

#[allow(
    clippy::panic,
    reason = "unexpected engine outcomes fail this test with their cause"
)]
fn failure(action: Action, error: &OperationError) -> Response {
    assert_eq!(
        error.outcome(),
        action.mutates().then_some(MutationOutcome::NotCommitted)
    );

    match error.failure() {
        OperationFailure::NoSuchBucket => Response::MissingBucket,
        OperationFailure::NoSuchKey => Response::MissingKey,
        OperationFailure::BucketNotEmpty => Response::NonemptyBucket,
        OperationFailure::PreconditionFailed => Response::Precondition,
        OperationFailure::StoreClosed => Response::Closed,
        other => panic!("unexpected history failure: {other:?}: {error}"),
    }
}

async fn invoke(
    connection: Connection,
    actor: usize,
    action: Action,
    history: Arc<History>,
    start: Arc<Barrier>,
) -> Recorded {
    let invoked = history.begin(actor, action);
    start.wait().await;

    let mut body = None;

    let result = match action {
        Action::CreateBucket => connection
            .create_bucket(BucketName::parse("attachments").unwrap())
            .await
            .map(|()| Response::Success),
        Action::DeleteBucket => connection
            .delete_bucket(BucketName::parse("attachments").unwrap(), None)
            .await
            .map(|()| Response::Success),
        Action::HeadBucket => connection
            .head_bucket(BucketName::parse("attachments").unwrap(), None)
            .await
            .map(|region| {
                assert_eq!(region, "us-east-1");
                Response::Success
            }),
        Action::Put(value, condition) => {
            let mut request = put(KEY);
            request.metadata = UserMetadata::new([("version".into(), value.to_string())]).unwrap();

            request.conditions = match condition {
                Condition::Any => WriteConditions::default(),
                Condition::Absent => WriteConditions::new(None, Some("*".into())).unwrap(),
                Condition::Matches(value) => WriteConditions::new(Some(etag(value)), None).unwrap(),
            };

            let bytes = payload(value);

            connection
                .put_object(request, io::Cursor::new(bytes))
                .await
                .map(|facts| {
                    assert_eq!(facts.etag(), etag(value));
                    Response::Success
                })
        }
        Action::Get => connection
            .get_object(read(KEY))
            .await
            .map(|(object, source)| {
                let object = selected(&object);
                body = Some((object, source));
                Response::Object(object)
            }),
        Action::Head => connection
            .head_object(read(KEY))
            .await
            .map(|object| Response::Object(selected(&object))),
        Action::Tags(value) => {
            let tags = ObjectTags::new(
                value
                    .into_iter()
                    .map(|value| ("status".into(), value.to_string())),
            )
            .unwrap();

            connection
                .replace_object_tags(read(KEY), tags)
                .await
                .map(|()| Response::Success)
        }
        Action::Delete(expected) => connection
            .delete_object(read(KEY), expected.map(etag))
            .await
            .map(|()| Response::Success),
        Action::Close => {
            connection.close().await.unwrap();
            Ok(Response::Success)
        }
    };

    let response = result.unwrap_or_else(|error| failure(action, &error));
    let entry = history.reply(actor, action, invoked, response);
    Recorded { entry, body }
}

async fn finish_body(recorded: &mut Recorded, history: &History) -> TestResult {
    let Some((object, mut body)) = recorded.body.take() else {
        return Ok(());
    };

    let expected = payload(object.value);
    let mut digest = Sha256::new();
    let mut length = 0;

    while let Some(chunk) = body.read_chunk(17_003).await? {
        digest.update(&chunk);
        length += chunk.len();
    }

    assert_eq!(length, expected.len());
    let hash = digest.finalize();
    assert_eq!(hash, Sha256::digest(&expected));
    body.close().await?;

    history.event(&serde_json::json!({
        "event": "body_eof", "invoked": recorded.entry.invoked,
        "at": history.clock.fetch_add(1, Ordering::SeqCst),
        "length": length, "sha256": hex(&hash),
    }));

    Ok(())
}

async fn batch(
    connections: &[Connection; 3],
    history: &Arc<History>,
    model: &mut Model,
    actions: &[(usize, Action)],
) -> TestResult {
    assert!(actions.len() <= 4);
    let start = Arc::new(Barrier::new(actions.len()));
    let mut tasks = JoinSet::new();

    for &(actor, action) in actions {
        tasks.spawn(invoke(
            connections[actor].clone(),
            actor,
            action,
            Arc::clone(history),
            Arc::clone(&start),
        ));
    }

    let mut records = Vec::new();

    let result = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(result) = tasks.join_next().await {
            records.push(result?);
        }

        Ok::<_, tokio::task::JoinError>(())
    })
    .await;

    if !matches!(result, Ok(Ok(()))) {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        result??;
    }

    // A non-overlapping observation constrains the batch's final register value.
    let observer = model.closed.iter().position(|closed| !closed).unwrap();

    records.push(
        invoke(
            connections[observer].clone(),
            observer,
            Action::Head,
            Arc::clone(history),
            Arc::new(Barrier::new(1)),
        )
        .await,
    );

    let entries = records
        .iter()
        .map(|recorded| recorded.entry)
        .collect::<Vec<_>>();

    let next = reconcile(*model, &entries);

    assert!(
        next.is_some(),
        "nonlinearizable history: {model:?} {entries:#?}"
    );

    *model = next.unwrap();

    // All changes in the batch have settled, but selected bodies still own their
    // original payload. The ordinary scheduler remains active throughout.
    for recorded in &mut records {
        finish_body(recorded, history).await?;
    }

    Ok(())
}

fn next_random(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn generated(state: &mut u64) -> Action {
    let value = u8::try_from(next_random(state) % 3 + 1).unwrap();

    match next_random(state) % 9 {
        0 => Action::Put(value, Condition::Any),
        1 => Action::Put(value, Condition::Absent),
        2 => Action::Put(value, Condition::Matches(1)),
        3 => Action::Get,
        4 => Action::Head,
        5 => Action::Tags(Some(value)),
        6 => Action::Tags(None),
        7 => Action::Delete(None),
        _ => Action::Delete(Some(value)),
    }
}

async fn open_connections(path: &std::path::Path) -> Result<[Connection; 3], ConnectionError> {
    Ok([
        Connection::open(path.to_path_buf(), None).await?,
        Connection::open(path.to_path_buf(), None).await?,
        Connection::open(path.to_path_buf(), None).await?,
    ])
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "qualification retains the owned history artifact"
)]
fn exhaustive_short_orders_and_seeded_histories_match_single_key_model() -> TestResult {
    let artifacts =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/histories");

    std::fs::create_dir_all(&artifacts)?;
    let directory = TestDirectory::new_in(&artifacts)?.retain_on_failure();
    let path = directory.path().join("store");

    let history = Arc::new(History {
        clock: AtomicUsize::new(0),
        log: Mutex::new(File::create_new(directory.path().join("history.jsonl"))?),
    });

    history.event(&serde_json::json!({
        "event": "fixture", "prng": "xorshift64 (13,7,17)",
        "scope": "single process, three connections, real storage, ordinary maintenance; invocation/response histories, not an internal checkpoint trace",
        "seeds": SEEDS,
        "rounds_per_seed": 48, "operations_per_round": 4,
        "payloads": (1..=3).map(|value| serde_json::json!({
            "value": value, "length": payload(value).len(), "etag": etag(value),
            "sha256": hex(&Sha256::digest(payload(value))),
        })).collect::<Vec<_>>(),
    }));

    runtime()?.block_on(async {
        let connections = open_connections(&path).await?;
        let mut model = Model::default();

        batch(
            &connections,
            &history,
            &mut model,
            &[(0, Action::CreateBucket)],
        )
        .await?;

        batch(
            &connections,
            &history,
            &mut model,
            &[(0, Action::Put(1, Condition::Any))],
        )
        .await?;

        let original_bucket = connections[0].head_object(read(KEY)).await?.bucket_id;

        history.event(&serde_json::json!({
            "event": "original_bucket", "bucket_id": hex(&original_bucket),
        }));

        let operations = [
            Action::Put(1, Condition::Absent),
            Action::Put(2, Condition::Absent),
            Action::Delete(None),
        ];

        // Enumerate all six orders of two create-only writes and deletion.
        for first in 0..3 {
            for second in 0..3 {
                if first == second {
                    continue;
                }

                let third = 3 - first - second;

                batch(
                    &connections,
                    &history,
                    &mut model,
                    &[(0, Action::Delete(None))],
                )
                .await?;

                for index in [first, second, third] {
                    batch(
                        &connections,
                        &history,
                        &mut model,
                        &[(index, operations[index])],
                    )
                    .await?;
                }
            }
        }

        for seed in SEEDS {
            let mut random = seed;

            for round in 0..48 {
                history.event(&serde_json::json!({"event": "round", "seed": seed, "round": round}));

                let actions =
                    std::array::from_fn::<_, 4, _>(|index| (index % 3, generated(&mut random)));

                batch(&connections, &history, &mut model, &actions).await?;
            }
        }

        batch(&connections, &history, &mut model, &[(0, Action::Close)]).await?;
        batch(&connections, &history, &mut model, &[(0, Action::Get)]).await?;

        batch(
            &connections,
            &history,
            &mut model,
            &[(1, Action::Delete(None))],
        )
        .await?;

        batch(
            &connections,
            &history,
            &mut model,
            &[(1, Action::DeleteBucket)],
        )
        .await?;

        batch(
            &connections,
            &history,
            &mut model,
            &[(2, Action::HeadBucket)],
        )
        .await?;

        batch(
            &connections,
            &history,
            &mut model,
            &[(2, Action::CreateBucket)],
        )
        .await?;

        batch(
            &connections,
            &history,
            &mut model,
            &[(2, Action::Put(3, Condition::Any))],
        )
        .await?;

        let replacement_bucket = connections[2].head_object(read(KEY)).await?.bucket_id;
        assert_ne!(replacement_bucket, original_bucket);

        history.event(&serde_json::json!({
            "event": "replacement_bucket", "bucket_id": hex(&replacement_bucket),
        }));

        for connection in &connections {
            connection.close().await?;
        }

        for _ in 0..2 {
            let reopened = open_connections(&path).await?;
            model.closed = [false; 3];
            batch(&reopened, &history, &mut model, &[(1, Action::Get)]).await?;

            assert_eq!(
                reopened[1].head_object(read(KEY)).await?.bucket_id,
                replacement_bucket
            );

            for connection in &reopened {
                connection.close().await?;
            }
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    history.event(&serde_json::json!({"event": "passed"}));
    drop(history);
    std::fs::remove_dir_all(&path)?;
    println!("HISTORY_ARTIFACT {}", directory.preserve().display());
    Ok(())
}
