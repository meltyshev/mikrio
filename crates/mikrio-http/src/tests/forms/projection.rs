//! Form field projection only; this sink never qualifies persistent publication.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use futures_util::stream;
use hyper::body::Bytes;
use s3s::dto::{PostObjectInput, PostObjectOutput};
use s3s::{S3Request, S3Response, S3Result};

use super::super::{Fixture, Service, TestResult, form, integrity};
use super::{Source, request};
use crate::access::REQUEST_KEY;

#[derive(Debug, PartialEq)]
struct Fields {
    acl: Option<String>,
    tagging: Option<String>,
    content_type: Option<String>,
    note: Option<String>,
}

struct Sink(Arc<Mutex<Vec<Fields>>>);

#[async_trait::async_trait]
impl s3s::S3 for Sink {
    async fn post_object(
        &self,
        request: S3Request<PostObjectInput>,
    ) -> S3Result<S3Response<PostObjectOutput>> {
        let fields = Fields {
            acl: request
                .input
                .acl
                .as_ref()
                .map(|acl| acl.as_str().to_owned()),
            tagging: request.input.tagging.clone(),
            content_type: request.input.content_type.clone(),
            note: request
                .input
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("note"))
                .cloned(),
        };

        let facts = integrity::consume_post(request).await?;
        self.0.lock().unwrap().push(fields);

        Ok(S3Response::new(PostObjectOutput {
            e_tag: Some(facts.etag().parse().unwrap()),
            ..PostObjectOutput::default()
        }))
    }
}

#[test]
fn post_fields_preserve_acl_xml_tags_and_empty_values_without_put_aliases() -> TestResult {
    Fixture::run(false, |fixture| {
        let observed = Arc::new(Mutex::new(Vec::new()));

        let service = Service::with_backend(
            fixture.connection.clone(),
            None,
            Sink(Arc::clone(&observed)),
        )?;

        fixture.runtime.block_on(async {
            for mode in [
                "form-fields",
                "form-fields-mixed-case",
                "form-fields-empty",
                "form-fields-absent",
                "form-put-acl",
                "form-put-tagging",
            ] {
                let raw = form(mode);

                let offset = raw
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                    .unwrap()
                    + 4;

                let body = Bytes::copy_from_slice(&raw[offset..]);
                let length = body.len();
                let request = request(Source(Box::pin(stream::iter([Ok(body)]))), Some(length));

                let response = REQUEST_KEY
                    .scope(RefCell::new(None), service.inner.call(request))
                    .await?;

                let expected = if mode.starts_with("form-put-") {
                    http::StatusCode::BAD_REQUEST
                } else {
                    http::StatusCode::NO_CONTENT
                };

                assert_eq!(response.status(), expected, "{mode}");
            }

            Ok::<_, Box<dyn std::error::Error>>(())
        })?;

        let mut fields = observed.lock().unwrap();
        assert_eq!(fields.len(), 4, "PUT-only aliases must not reach the sink");

        let absent = fields.pop().unwrap();
        assert_eq!(absent.acl, None);
        assert_eq!(absent.tagging, None);
        assert_eq!(absent.note, None);
        assert_eq!(absent.content_type, None);

        let empty = fields.pop().unwrap();
        assert_eq!(empty.acl.as_deref(), Some("bucket-owner-full-control"));
        assert_eq!(empty.tagging.as_deref(), Some(""));
        assert_eq!(empty.note.as_deref(), Some(""));
        assert_eq!(empty.content_type.as_deref(), Some("image/png"));

        assert_eq!(fields[0], fields[1], "field names are case insensitive");
        assert_eq!(fields[0].acl.as_deref(), Some("bucket-owner-full-control"));

        assert_eq!(
            fields[0].tagging.as_deref(),
            Some(
                "<Tagging><TagSet><Tag><Key>Label</Key><Value>café</Value></Tag></TagSet></Tagging>"
            )
        );

        assert_eq!(fields[0].content_type.as_deref(), Some("image/png"));
        assert_eq!(fields[0].note.as_deref(), Some("form metadata"));
        drop(fields);
        Ok(())
    })
}
