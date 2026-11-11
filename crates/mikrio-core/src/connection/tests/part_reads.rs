//! Selected extents keep ordinary reader ownership and whole-file validation.

use super::objects::{put, read};
use super::read_authorization::seed_parts;
use super::*;
use crate::OperationFailure;
use crate::multipart::PartNumber;
use crate::object::{BucketName, ReadRequest};

fn selected() -> ReadRequest {
    let mut request = read("selected");
    request.part_number = Some(PartNumber::new(2).unwrap());
    request
}

#[test]
fn part_reads_keep_independent_cursors_across_replacement_delete_and_gc() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        seed_parts(&connection, put("selected"), b"old bytes").await?;
        let (facts, mut first) = connection.get_object(selected()).await?;
        let (_, mut second) = connection.get_object(selected()).await?;
        assert_eq!(first.read_chunk(3).await?, Some(b"old".to_vec()));
        assert_eq!(second.read_chunk(4).await?, Some(b"old ".to_vec()));

        connection
            .put_object(put("selected"), b"replacement".as_slice())
            .await?;

        connection.delete_object(read("selected"), None).await?;
        let blob = facts.blob_id;

        let payload_path = path
            .join("objects")
            .join(format!("{:02x}", blob.shard()))
            .join(blob.to_string());

        super::super::maintenance::batch(Arc::new(connection.admit()?), path.clone(), blob.shard())
            .await?;

        assert!(payload_path.exists());
        assert_eq!(second.read_all(5).await?, b"bytes");
        second.close().await?;
        assert_eq!(first.read_chunk(1).await?, Some(b" ".to_vec()));
        first.close().await?;

        super::super::maintenance::batch(Arc::new(connection.admit()?), path, blob.shard()).await?;

        assert!(!payload_path.exists());
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn part_reads_allow_empty_final_extent_and_validate_whole_file_length() -> TestResult {
    let directory = TestDirectory::new()?.retain_on_failure();

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        connection.pause_maintenance();

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        seed_parts(&connection, put("selected"), b"").await?;
        let (facts, mut body) = connection.get_object(selected()).await?;
        assert_eq!(facts.content_offset(), 5 * 1024 * 1024);
        assert_eq!(facts.content_length(), 0);
        assert_eq!(body.read_chunk(1).await?, None);
        body.close().await?;
        let head = connection.head_object(selected()).await?;
        assert_eq!(head.content_length(), 0);

        let payload_path = path
            .join("objects")
            .join(format!("{:02x}", facts.blob_id.shard()))
            .join(facts.blob_id.to_string());

        std::fs::OpenOptions::new()
            .write(true)
            .open(payload_path)?
            .set_len(5 * 1024 * 1024 - 1)?;

        // HEAD remains metadata-only even when an empty selected part needs no
        // bytes; GET still validates the complete immutable file before opening.
        assert_eq!(
            connection.head_object(selected()).await?.content_length(),
            0
        );

        let error = connection
            .get_object(selected())
            .await
            .err()
            .ok_or("empty selection bypassed full-file validation")?;

        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);

        seed_parts(&connection, put("selected"), b"tail").await?;
        let (facts, mut body) = connection.get_object(selected()).await?;

        let payload_path = path
            .join("objects")
            .join(format!("{:02x}", facts.blob_id.shard()))
            .join(facts.blob_id.to_string());

        // Simulate out-of-protocol truncation after the initial file check.
        std::fs::OpenOptions::new()
            .write(true)
            .open(payload_path)?
            .set_len(facts.content_offset())?;

        assert_eq!(
            body.read_chunk(4).await.unwrap_err().failure(),
            crate::body::BodyFailure::Io
        );

        body.close().await?;
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
