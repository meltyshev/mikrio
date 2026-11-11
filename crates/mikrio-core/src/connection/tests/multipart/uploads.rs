use super::*;
use crate::multipart::{ListUploadsRequest, UploadPageSize, UploadRequest};

fn page() -> ListUploadsRequest {
    ListUploadsRequest {
        bucket: request().bucket,
        expected_owner: None,
        prefix: None,
        delimiter: None,
        key_marker: None,
        upload_id_marker: None,
        max_uploads: UploadPageSize::new(1).unwrap(),
    }
}

#[test]
fn upload_pages_resume_after_abort_and_reopen_and_exclude_retired_bucket_identities() -> TestResult
{
    let directory = TestDirectory::new()?.retain_on_failure();
    let path = directory.path().join("store");

    runtime()?.block_on(async {
        let connection = Connection::open(path.clone(), None).await?;
        let peer = Connection::open(path.clone(), None).await?;
        connection.create_bucket(request().bucket).await?;

        for key in ["a", "z"] {
            connection
                .create_multipart_upload(CreateRequest {
                    key: crate::object::ObjectKey::parse(key)?,
                    ..request()
                })
                .await?;
        }

        let first = connection.list_multipart_uploads(page()).await?;
        assert_eq!(first.uploads.len(), 1);
        assert_eq!(first.uploads[0].key, "a");
        let marker = first.next_marker.unwrap();

        peer.abort_multipart_upload(UploadRequest {
            bucket: request().bucket,
            key: crate::object::ObjectKey::parse("a")?,
            upload_id: first.uploads[0].upload_id.clone(),
            expected_owner: None,
        })
        .await?;

        for key in ["0-before", "a/after"] {
            peer.create_multipart_upload(CreateRequest {
                key: crate::object::ObjectKey::parse(key)?,
                ..request()
            })
            .await?;
        }

        connection.close().await?;
        let reopened = Connection::open(path.clone(), None).await?;
        let mut request = page();
        request.key_marker = Some(marker.key);
        request.upload_id_marker = marker.upload_id;
        let second = reopened.list_multipart_uploads(request.clone()).await?;
        assert_eq!(second.uploads[0].key, "a/after");
        let marker = second.next_marker.unwrap();
        request.key_marker = Some(marker.key);
        request.upload_id_marker = marker.upload_id;
        let third = reopened.list_multipart_uploads(request).await?;
        assert_eq!(third.uploads[0].key, "z");
        assert_eq!(third.next_marker, None);
        peer.delete_bucket(page().bucket, None).await?;
        peer.create_bucket(page().bucket).await?;

        assert_eq!(
            reopened.list_multipart_uploads(page()).await?.uploads,
            Vec::new()
        );

        reopened.close().await?;
        peer.close().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    directory.finish()?;
    Ok(())
}
