//! Bounded physical completion and cleanup in an explicitly owned process.

use std::pin::Pin;
use std::task::Context;
use std::time::Instant;

use sha2::Digest as _;
use tokio::io::{AsyncRead, ReadBuf};

use super::*;

const PART_BYTES: u64 = 5 * 1024 * 1024;
const PART_COUNT: u32 = 64;
const CHUNK_BYTES: usize = 64 * 1024;
const PAYLOAD_BYTES: u64 = 320 * 1024 * 1024;
// Independently generated with Python hashlib over byte(n) repeated 5 MiB,
// for n = 1..64; ETag hashes the concatenated binary part MD5 digests.
const SHA256: &str = "6edabf8e8fb03cf819d4cc4e0a68feafc6296759c2386a562fc41926bd032a93";
const ETAG: &str = "\"cdd325d91f463a7e8b9bc7ce074dfb11-64\"";

struct PartSource {
    chunk: Vec<u8>,
    remaining: u64,
}

impl AsyncRead for PartSource {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let length = buffer
            .remaining()
            .min(self.chunk.len())
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));

        buffer.put_slice(&self.chunk[..length]);
        self.remaining -= u64::try_from(length).map_err(io::Error::other)?;
        Poll::Ready(Ok(()))
    }
}

fn footprint(path: &std::path::Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    // The caller closes every engine connection before inspecting owned files.
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut allocated_bytes = 0_u64;

    for tree in ["staging", "objects"] {
        for shard in std::fs::read_dir(path.join(tree))? {
            for entry in std::fs::read_dir(shard?.path())? {
                let entry = entry?;
                let metadata = entry.metadata()?;
                assert!(metadata.is_file());
                files += 1;
                bytes += metadata.len();

                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt as _;
                    allocated_bytes += metadata.blocks() * 512;
                }

                #[cfg(windows)]
                {
                    allocated_bytes += crate::windows::allocated_bytes(&entry.path())?;
                }
            }
        }
    }

    Ok(serde_json::json!({
        "files": files, "bytes": bytes, "allocated_bytes": allocated_bytes,
    }))
}

fn pending(path: &std::path::Path) -> Result<i64, Box<dyn std::error::Error>> {
    let database = rusqlite::Connection::open_with_flags(
        path.join("metadata.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    let count = database.query_row(
        "SELECT (SELECT count(*) FROM blobs WHERE gc_pending=1)
         + (SELECT count(*) FROM multipart_receipts)
         + (SELECT count(*) FROM multipart_parts)
         + (SELECT count(*) FROM multipart_uploads)",
        [],
        |row| row.get(0),
    )?;

    database.close().map_err(|(_, error)| error)?;
    Ok(count)
}

async fn readback(connection: &Connection) -> TestResult {
    let (selected, mut body) = connection
        .get_object(super::super::super::objects::read(request().key.as_str()))
        .await?;

    assert_eq!(selected.length.get(), PAYLOAD_BYTES);
    assert_eq!(selected.etag, ETAG);
    let mut digest = sha2::Sha256::new();
    let mut received = 0_u64;

    while let Some(chunk) = body.read_chunk(CHUNK_BYTES).await? {
        assert!(chunk.len() <= CHUNK_BYTES);

        for byte in &chunk {
            assert_eq!(u64::from(*byte), received / PART_BYTES + 1);
            received += 1;
        }

        digest.update(&chunk);
    }

    body.close().await?;
    assert_eq!(received, PAYLOAD_BYTES);

    assert_eq!(
        digest.finalize().as_slice(),
        crate::values::decode_hex::<32>(SHA256.as_bytes())?
    );

    Ok(())
}

#[test]
#[ignore = "Controlled subprocess entry for physical multipart workload measurements"]
fn physical_completion_child() -> TestResult {
    let path = PathBuf::from(
        std::env::var_os("mikrio_probe_directory").ok_or("missing measurement directory")?,
    );

    if std::env::var("mikrio_probe_mode")? != "5242880:64:1:1" || path.exists() {
        return Err("physical completion requires its exact profile and a fresh store".into());
    }

    let started = Instant::now();

    let record = runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();
        connection.create_bucket(request().bucket).await?;

        connection
            .put_object(
                super::super::super::objects::put(request().key.as_str()),
                PartSource {
                    chunk: vec![0; CHUNK_BYTES],
                    remaining: u64::try_from(CHUNK_BYTES)?,
                },
            )
            .await?;

        let (_, mut predecessor) = connection
            .get_object(super::super::super::objects::read(request().key.as_str()))
            .await?;

        let created = connection.create_multipart_upload(request()).await?;
        let mut part_ns = Vec::new();

        for number in 1..=PART_COUNT {
            let part_started = Instant::now();

            connection
                .upload_part(
                    super::super::parts::part(created.upload_id.clone(), number),
                    PartSource {
                        chunk: vec![u8::try_from(number)?; CHUNK_BYTES],
                        remaining: PART_BYTES,
                    },
                )
                .await?;

            part_ns.push(part_started.elapsed().as_nanos());
        }

        let input = completion(&connection, created.upload_id).await?;
        assert_eq!(input.parts.parts().len(), usize::try_from(PART_COUNT)?);
        let completion_started = Instant::now();
        let completed = connection.complete_multipart_upload(input).await?;
        let completion_ns = completion_started.elapsed().as_nanos();
        assert_eq!(completed.length.get(), PAYLOAD_BYTES);
        assert_eq!(completed.etag, ETAG);
        let mut predecessor_bytes = 0;

        while let Some(chunk) = predecessor.read_chunk(CHUNK_BYTES).await? {
            assert!(chunk.iter().all(|byte| *byte == 0));
            predecessor_bytes += chunk.len();
        }

        assert_eq!(predecessor_bytes, CHUNK_BYTES);
        predecessor.close().await?;
        connection.close().await?;
        let before_cleanup = footprint(&path)?;
        assert_eq!(before_cleanup["files"], PART_COUNT + 2);

        assert_eq!(
            before_cleanup["bytes"],
            2 * PAYLOAD_BYTES + u64::try_from(CHUNK_BYTES)?
        );

        let cleanup_started = Instant::now();

        tokio::time::timeout(Duration::from_secs(60), async {
            while pending(&path)? != 0 {
                let worker = Connection::open(path.clone(), None).await?;
                tokio::time::sleep(Duration::from_millis(20)).await;
                worker.close().await?;
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await??;

        let cleanup_ns = cleanup_started.elapsed().as_nanos();
        let after_cleanup = footprint(&path)?;
        assert_eq!(after_cleanup["files"], 1);
        assert_eq!(after_cleanup["bytes"], PAYLOAD_BYTES);

        let reopened = Connection::open(path.clone(), None).await?;
        let read_started = Instant::now();
        readback(&reopened).await?;
        let read_ns = read_started.elapsed().as_nanos();
        reopened.close().await?;

        let database = rusqlite::Connection::open_with_flags(
            path.join("metadata.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;

        let mut query = database.prepare(
            "SELECT part_number, byte_offset, byte_length FROM blob_parts ORDER BY part_number",
        )?;

        let layout = query
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        assert_eq!(
            layout,
            (1..=PART_COUNT)
                .map(|number| (number, i64::from(number - 1) * 5 * 1024 * 1024, 5 * 1024 * 1024))
                .collect::<Vec<_>>()
        );

        drop(query);
        database.close().map_err(|(_, error)| error)?;

        Ok::<_, Box<dyn std::error::Error>>(serde_json::json!({
            "scope": "internal worker in a test build; one physical completion, held predecessor, closed/reopened read and settled cleanup; no public API or maximum-scale qualification",
            "part_bytes": PART_BYTES, "part_count": PART_COUNT, "concurrency": 1, "rounds": 1,
            "source_chunk_bytes": CHUNK_BYTES, "returned_chunk_max_bytes": CHUNK_BYTES,
            "payload_bytes": PAYLOAD_BYTES, "sha256": SHA256, "etag": ETAG,
            "predecessor_bytes": predecessor_bytes, "retained_part_facts": layout.len(),
            "pending_cleanup": pending(&path)?,
            "before_cleanup": before_cleanup, "after_cleanup": after_cleanup,
            "part_ns": part_ns, "completion_ns": completion_ns,
            "read_ns": read_ns, "cleanup_ns": cleanup_ns,
            "elapsed_ns": started.elapsed().as_nanos(),
        }))
    })?;

    let mut output = std::io::stdout().lock();
    writeln!(output, "PROBE {record}")?;
    output.flush()?;
    drop(output);
    let mut control = String::new();
    std::io::stdin().lock().read_line(&mut control)?;

    if control != "exit\n" {
        return Err("measurement control pipe closed".into());
    }

    Ok(())
}
