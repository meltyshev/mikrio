use super::*;
use crate::body::BodyFailure;
use crate::integrity::{PutIntegrity, PutIntegrityInput};
use crate::object::{
    BucketName, ByteLength, ObjectKey, PutRequest, ReadConditions, ReadConditionsInput,
    ReadRequest, Representation, RepresentationInput, UserMetadata, WriteConditions,
};
use crate::{MutationOutcome, OperationFailure};
use std::pin::Pin;
use std::task::Context as TaskContext;
use tokio::io::{AsyncRead, ReadBuf};

pub(super) fn put(key: &str) -> PutRequest {
    PutRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse(key).unwrap(),
        expected_owner: None,
        content_length: None,
        representation: Representation::new(RepresentationInput::default()).unwrap(),
        metadata: UserMetadata::default(),
        tags: None,
        integrity: PutIntegrity::new(PutIntegrityInput::default()).unwrap(),
        conditions: WriteConditions::new(None, None).unwrap(),
    }
}

pub(super) fn read(key: &str) -> ReadRequest {
    ReadRequest {
        bucket: BucketName::parse("attachments").unwrap(),
        key: ObjectKey::parse(key).unwrap(),
        expected_owner: None,
        conditions: ReadConditions::new(ReadConditionsInput::default()).unwrap(),
        range: None,
        part_number: None,
        response: crate::object::Representation::default(),
    }
}

#[test]
fn protected_generations_have_independent_cursors_and_automatic_reclamation() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        let first = connection
            .put_object(put("a/../old"), b"old bytes".as_slice())
            .await?;

        assert_eq!(first.etag(), "\"125270c450105b4a49e9421ef42e0b53\"");
        let (selected, mut body) = connection.get_object(read("a/../old")).await?;
        let (_, mut second) = connection.get_object(read("a/../old")).await?;
        assert_eq!(body.read_chunk(3).await?, Some(b"old".to_vec()));
        assert_eq!(second.read_chunk(4).await?, Some(b"old ".to_vec()));

        connection
            .put_object(put("a/../old"), b"new bytes".as_slice())
            .await?;

        connection.delete_object(read("a/../old"), None).await?;

        assert_eq!(
            connection
                .head_object(read("a/../old"))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchKey
        );

        let previous_payload_path = path
            .join("objects")
            .join(format!("{:02x}", selected.blob_id.shard()))
            .join(selected.blob_id.to_string());

        let manager = GuardManager::open(&path)?;

        assert!(
            manager
                .try_acquire(
                    GuardTarget::Shard(selected.blob_id.shard()),
                    GuardMode::Exclusive
                )?
                .is_none()
        );

        assert!(previous_payload_path.exists());
        assert_eq!(second.read_all(99).await?, b"bytes");
        assert_eq!(body.read_all(99).await?, b" bytes");

        tokio::time::timeout(Duration::from_secs(20), async {
            while previous_payload_path.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;

        connection.close().await?;
        let reopened = Connection::open(path, None).await?;

        assert_eq!(
            reopened
                .head_object(read("a/../old"))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchKey
        );

        reopened.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

struct Fragmented(std::collections::VecDeque<Vec<u8>>);

impl AsyncRead for Fragmented {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(chunk) = self.0.front_mut() else {
            return Poll::Ready(Ok(()));
        };

        let length = buffer.remaining().min(chunk.len());
        buffer.put_slice(&chunk[..length]);
        chunk.drain(..length);

        if chunk.is_empty() {
            self.0.pop_front();
        }

        Poll::Ready(Ok(()))
    }
}

#[test]
fn fragmented_upload_preserves_exact_bytes_across_reused_buffer() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        let chunks = [vec![0x11; 65_536], vec![0x7d; 17], vec![0xf3; 65_539]];
        let expected = chunks.concat();
        let source = Fragmented(chunks.into());

        connection.put_object(put("fragmented"), source).await?;
        let (selected, mut body) = connection.get_object(read("fragmented")).await?;
        assert_eq!(selected.length.get(), u64::try_from(expected.len())?);
        assert_eq!(body.read_all(expected.len()).await?, expected);
        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

struct Failing;
impl AsyncRead for Failing {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut TaskContext<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("owned test source failure")))
    }
}

#[test]
fn failed_producers_integrity_and_conditions_preserve_the_predecessor() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let connection = Connection::open(directory.path().join("store"), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("key"), b"original".as_slice())
            .await?;

        let error = connection
            .put_object(put("key"), Failing)
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::BodyReadFailed);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        let mut request = put("key");
        request.content_length = Some(ByteLength::for_put(100)?);

        let error = connection
            .put_object(request, b"short".as_slice())
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::IncompleteBody);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));
        let mut request = put("key");

        request.integrity = PutIntegrity::new(PutIntegrityInput {
            content_md5: Some("1B2M2Y8AsgTpgAmY7PhCfg==".into()),
            ..Default::default()
        })?;

        let error = connection
            .put_object(request, b"wrong digest".as_slice())
            .await
            .unwrap_err();

        assert_eq!(error.failure(), OperationFailure::BadDigest);
        let mut request = put("key");
        request.conditions = WriteConditions::new(None, Some("*".into()))?;

        assert_eq!(
            connection
                .put_object(request, b"conflict".as_slice())
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::PreconditionFailed
        );

        let (_, mut body) = connection.get_object(read("key")).await?;
        assert_eq!(body.read_all(100).await?, b"original");

        assert_eq!(
            connection
                .delete_bucket(BucketName::parse("attachments")?, None)
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::BucketNotEmpty
        );

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

struct Stalled(Option<oneshot::Sender<()>>);
impl AsyncRead for Stalled {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut TaskContext<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(sender) = self.0.take() {
            let _delivery = sender.send(());
        }

        Poll::Pending
    }
}

#[test]
fn close_cancels_stalled_source_and_idle_body_without_closing_peer() -> TestResult {
    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path, None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("key"), b"durable".as_slice())
            .await?;

        let (_, mut body) = connection.get_object(read("key")).await?;
        let (entered, observed) = oneshot::channel();
        let writer = connection.clone();

        let task = tokio::spawn(async move {
            writer
                .put_object(put("stalled"), Stalled(Some(entered)))
                .await
        });

        observed.await?;
        tokio::time::timeout(Duration::from_secs(10), connection.close()).await??;
        let error = task.await?.unwrap_err();
        assert_eq!(error.failure(), OperationFailure::OperationCanceled);
        assert_eq!(error.outcome(), Some(MutationOutcome::NotCommitted));

        assert_eq!(
            body.read_chunk(1).await.unwrap_err().failure(),
            BodyFailure::Closed
        );

        body.close().await?;
        let (_, mut current) = peer.get_object(read("key")).await?;
        assert_eq!(current.read_all(100).await?, b"durable");

        assert_eq!(
            peer.head_object(read("stalled"))
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::NoSuchKey
        );

        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}

#[test]
fn ranges_keep_independent_protected_cursors_and_head_never_opens_payloads() -> TestResult {
    use crate::object::ByteRange;

    let directory = TestDirectory::new()?;

    runtime()?.block_on(async {
        let path = directory.path().join("store");
        let connection = Connection::open(path.clone(), None).await?;

        connection
            .create_bucket(BucketName::parse("attachments")?)
            .await?;

        connection
            .put_object(put("range"), b"testcontent".as_slice())
            .await?;

        let mut request = read("range");
        request.range = Some(ByteRange::parse("bytes=4-7")?);
        let (selected, mut first) = connection.get_object(request.clone()).await?;
        let (_, mut second) = connection.get_object(request.clone()).await?;
        assert_eq!(selected.length.get(), 11);
        assert_eq!(selected.content_length(), 4);
        assert_eq!(selected.content_range().as_deref(), Some("bytes 4-7/11"));
        assert!(selected.checksums.is_empty());
        assert_eq!(first.read_chunk(1).await?, Some(b"c".to_vec()));
        assert_eq!(second.read_chunk(2).await?, Some(b"co".to_vec()));

        connection
            .put_object(put("range"), b"replacement".as_slice())
            .await?;

        connection.delete_object(read("range"), None).await?;
        let manager = GuardManager::open(&path)?;

        assert!(
            manager
                .try_acquire(
                    GuardTarget::Shard(selected.blob_id.shard()),
                    GuardMode::Exclusive
                )?
                .is_none()
        );

        assert_eq!(second.read_all(20).await?, b"nt");
        assert_eq!(first.read_all(20).await?, b"ont");
        first.close().await?;
        second.close().await?;

        connection
            .put_object(put("range"), b"testcontent".as_slice())
            .await?;

        let selected = connection.head_object(read("range")).await?;

        let payload_path = path
            .join("objects")
            .join(format!("{:02x}", selected.blob_id.shard()))
            .join(selected.blob_id.to_string());

        // Only a test corrupts a published payload. HEAD must remain metadata-only;
        // GET must validate the full file even if the requested prefix still fits.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&payload_path)?
            .set_len(8)?;

        assert_eq!(
            connection
                .head_object(request.clone())
                .await?
                .content_length(),
            4
        );

        let error = connection
            .get_object(request.clone())
            .await
            .err()
            .ok_or("range hid a truncated payload")?;

        assert_eq!(error.failure(), OperationFailure::StoreCorrupt);
        request.range = Some(ByteRange::parse("bytes=40-50")?);

        assert_eq!(
            connection
                .head_object(request.clone())
                .await
                .unwrap_err()
                .failure(),
            OperationFailure::InvalidRange
        );

        request.conditions = ReadConditions::new(ReadConditionsInput {
            if_none_match: Some(selected.etag),
            ..ReadConditionsInput::default()
        })?;

        assert_eq!(
            connection.head_object(request).await.unwrap_err().failure(),
            OperationFailure::NotModified
        );

        assert!(
            manager
                .try_acquire(
                    GuardTarget::Shard(selected.blob_id.shard()),
                    GuardMode::Exclusive
                )?
                .is_some()
        );

        connection.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
