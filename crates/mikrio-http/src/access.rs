//! Correlate verified credentials with one request, then consult current SQL.

use std::cell::RefCell;
use std::sync::Arc;

use mikrio_core::access::{Action, Bootstrap, Context, CredentialClass, HttpIdentity, SigningKey};
use mikrio_core::object::{
    BucketName, ObjectKey, Representation, RepresentationInput, UserMetadata,
};
use mikrio_core::{AccessError, AccessRequest, Connection};
use s3s::access::{S3Access, S3AccessContext};
use s3s::auth::{S3Auth, SecretKey};
use s3s::{S3Error, S3Result, s3_error};

tokio::task_local! {
    pub(super) static REQUEST_KEY: RefCell<Option<(String, SigningKey)>>;
}

#[derive(Clone)]
pub(super) struct Access {
    connection: Connection,
    bootstrap: Option<Arc<Bootstrap>>,
    #[cfg(test)]
    observations: bool,
}

impl Access {
    pub(super) const fn new(connection: Connection, bootstrap: Option<Arc<Bootstrap>>) -> Self {
        Self {
            connection,
            bootstrap,
            #[cfg(test)]
            observations: false,
        }
    }

    #[cfg(test)]
    pub(super) fn observations(mut self) -> Self {
        self.observations = true;
        self
    }
}

#[async_trait::async_trait]
impl S3Auth for Access {
    async fn get_secret_key(&self, key: &str) -> S3Result<SecretKey> {
        let signing =
            match CredentialClass::parse(key).map_err(|_| s3_error!(InvalidAccessKeyId))? {
                CredentialClass::Application => self
                    .connection
                    .resolve_application_key(key.to_owned())
                    .await
                    .map_err(|error| access_error(&error))?,
                CredentialClass::Bootstrap => self
                    .bootstrap
                    .as_ref()
                    .and_then(|bootstrap| bootstrap.resolve(key))
                    .ok_or_else(|| s3_error!(InvalidAccessKeyId))?,
            };

        let secret = SecretKey::from(signing.secret().expose());

        REQUEST_KEY
            .try_with(|slot| {
                let mut slot = slot.borrow_mut();

                if slot.is_some() {
                    return Err(s3_error!(AccessDenied));
                }

                *slot = Some((key.to_owned(), signing));
                Ok(())
            })
            .map_err(|_| s3_error!(InternalError))??;

        Ok(secret)
    }
}

#[async_trait::async_trait]
impl S3Access for Access {
    async fn check(&self, cx: &mut S3AccessContext<'_>) -> S3Result<()> {
        let signing = REQUEST_KEY
            .try_with(|slot| slot.borrow_mut().take())
            .map_err(|_| s3_error!(InternalError))?;

        let identity = match (cx.credentials(), signing) {
            (None, None) => HttpIdentity::anonymous(),
            (Some(credentials), Some((key, signing))) if credentials.access_key == key => {
                signing.verified()
            }
            _ => return Err(s3_error!(AccessDenied)),
        };

        if cx.headers().contains_key("x-amz-checksum-mode")
            && !matches!(cx.s3_op().name(), "GetObject" | "HeadObject")
        {
            return Err(s3_error!(NotImplemented));
        }

        // Full batches, 10,000 parts and 1,000 escaped lifecycle rules
        // need the bounded 8 MiB configuration envelope.
        let body_limit = if matches!(
            cx.s3_op().name(),
            "DeleteObjects" | "CompleteMultipartUpload" | "PutBucketLifecycleConfiguration"
        ) {
            8 * 1024 * 1024
        } else {
            64 * 1024
        };

        cx.extensions_mut()
            .insert(s3s::service::RequestBodyLimit(body_limit));

        #[cfg(test)]
        let observations = self.observations;

        #[cfg(not(test))]
        let observations = false;

        if cx.s3_op().name() == "PostObject" {
            if cx.credentials().is_none_or(|credentials| {
                CredentialClass::parse(&credentials.access_key) != Ok(CredentialClass::Application)
            }) {
                return Err(s3_error!(AccessDenied));
            }

            // The file key is available only after form input conversion. Its
            // storage worker authorizes before reservation and again at publication.
            cx.extensions_mut().insert(Arc::new(identity));
            return Ok(());
        }

        if !observations {
            if !matches!(
                cx.s3_op().name(),
                "GetObject"
                    | "CopyObject"
                    | "CreateMultipartUpload"
                    | "UploadPart"
                    | "UploadPartCopy"
                    | "GetObjectAttributes"
                    | "CompleteMultipartUpload"
                    | "AbortMultipartUpload"
                    | "ListParts"
                    | "ListMultipartUploads"
                    | "HeadObject"
                    | "PutObject"
                    | "DeleteObject"
                    | "DeleteObjects"
                    | "CreateBucket"
                    | "HeadBucket"
                    | "ListObjectsV2"
                    | "ListBuckets"
                    | "GetBucketLocation"
                    | "DeleteBucket"
                    | "PutBucketLifecycleConfiguration"
                    | "GetBucketLifecycleConfiguration"
                    | "DeleteBucketLifecycle"
                    | "PutBucketCors"
                    | "GetBucketCors"
                    | "DeleteBucketCors"
                    | "PutBucketPolicy"
                    | "GetBucketPolicy"
                    | "DeleteBucketPolicy"
                    | "GetBucketPolicyStatus"
                    | "PutPublicAccessBlock"
                    | "GetPublicAccessBlock"
                    | "DeletePublicAccessBlock"
                    | "PutBucketOwnershipControls"
                    | "GetBucketOwnershipControls"
                    | "GetBucketAcl"
                    | "GetObjectAcl"
                    | "GetObjectTagging"
                    | "PutObjectTagging"
                    | "DeleteObjectTagging"
            ) {
                return Err(s3_error!(NotImplemented));
            }

            if cx.s3_op().name() == "ListBuckets" {
                if !cx.s3_path().is_root() {
                    return Err(s3_error!(AccessDenied));
                }
            } else if let Some((bucket, key)) = cx.s3_path().as_object() {
                validate_names(bucket, key)?;
            } else {
                let bucket = cx
                    .s3_path()
                    .as_bucket()
                    .ok_or_else(|| s3_error!(AccessDenied))?;

                BucketName::parse(bucket).map_err(|_| s3_error!(InvalidBucketName))?;
            }

            // The codec drops empty optional strings. Validate the raw owner
            // assertion first so a supplied empty value cannot become absence.
            expected_owner(cx.headers())?;

            if cx.headers().contains_key("x-amz-meta-") {
                return Err(s3_error!(InvalidArgument));
            }

            // Storage authorizes with current object tags and conditions in its
            // selection/reservation transaction, then again at publication.
            cx.extensions_mut().insert(Arc::new(identity));
            return Ok(());
        }

        // Unknown operations are rejected before backend dispatch.
        let actions: &[&str] = match cx.s3_op().name() {
            "GetObject" | "HeadObject" => &["s3:GetObject"],
            "GetObjectAttributes" => &["s3:GetObject", "s3:GetObjectAttributes"],
            "PutObject" => &["s3:PutObject"],
            _ => return Err(s3_error!(NotImplemented)),
        };

        let (bucket, key) = cx
            .s3_path()
            .as_object()
            .ok_or_else(|| s3_error!(AccessDenied))?;

        validate_names(bucket, key)?;

        if cx.headers().contains_key("x-amz-meta-") {
            return Err(s3_error!(InvalidArgument));
        }

        let context = Context::default();
        let expected_owner = expected_owner(cx.headers())?;
        let mut requests = Vec::with_capacity(actions.len());

        for action in actions {
            let request = AccessRequest {
                bucket,
                expected_bucket_id: None,
                key: Some(key),
                action: Action::parse(action).map_err(|_| s3_error!(InternalError))?,
                context: &context,
                expected_owner,
            }
            .into_owned()
            .map_err(|_| s3_error!(InvalidArgument))?;

            requests.push(request);
        }

        let identity = Arc::new(identity);

        self.connection
            .authorize_http(Arc::clone(&identity), requests)
            .await
            .map_err(|error| access_error(&error))?;

        cx.extensions_mut().insert(identity);

        Ok(())
    }

    async fn put_object(
        &self,
        request: &mut s3s::S3Request<s3s::dto::PutObjectInput>,
    ) -> S3Result<()> {
        let input = &request.input;
        validate_metadata(input.metadata.as_ref())?;

        Representation::new(RepresentationInput {
            content_type: input.content_type.clone(),
            content_encoding: input.content_encoding.clone(),
            content_disposition: input.content_disposition.clone(),
            content_language: input.content_language.clone(),
            cache_control: input.cache_control.clone(),
            expires: parse_expires(input.expires.as_deref())?,
        })
        .map_err(|_| s3_error!(InvalidArgument))?;

        Ok(())
    }

    async fn post_object(
        &self,
        request: &mut s3s::S3Request<s3s::dto::PostObjectInput>,
    ) -> S3Result<()> {
        let input = &request.input;
        validate_names(&input.bucket, &input.key)?;
        validate_metadata(input.metadata.as_ref())?;

        Representation::new(RepresentationInput {
            content_type: input.content_type.clone(),
            content_encoding: input.content_encoding.clone(),
            content_disposition: input.content_disposition.clone(),
            content_language: input.content_language.clone(),
            cache_control: input.cache_control.clone(),
            expires: parse_expires(input.expires.as_deref())?,
        })
        .map_err(|_| s3_error!(InvalidArgument))?;

        #[cfg(test)]
        if self.observations {
            return self.authorize_post_observation(request).await;
        }

        // Production authorization uses the operation-owned creation/tag context
        // inside the existing PUT reservation and publication transactions.
        Ok(())
    }
}

pub(super) fn expected_owner(headers: &http::HeaderMap) -> S3Result<Option<&str>> {
    let owner = headers
        .get("x-amz-expected-bucket-owner")
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| s3_error!(InvalidArgument))?;

    if let Some(owner) = owner {
        mikrio_core::access::validate_account_id(owner).map_err(|_| s3_error!(InvalidArgument))?;
    }

    Ok(owner)
}

fn validate_names(bucket: &str, key: &str) -> S3Result<()> {
    BucketName::parse(bucket).map_err(|_| s3_error!(InvalidBucketName))?;
    ObjectKey::parse(key).map_err(|_| s3_error!(InvalidArgument))?;
    Ok(())
}

fn validate_metadata(metadata: Option<&s3s::dto::Metadata>) -> S3Result<()> {
    let Some(metadata) = metadata else {
        return Ok(());
    };

    UserMetadata::new(
        metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    )
    .map_err(|_| s3_error!(InvalidArgument))?;

    Ok(())
}

pub(super) fn parse_expires(value: Option<&str>) -> S3Result<Option<mikrio_core::EpochMillis>> {
    use s3s::dto::{Timestamp, TimestampFormat};

    let Some(value) = value else { return Ok(None) };

    let parsed = Timestamp::parse(TimestampFormat::HttpDate, value)
        .map_err(|_| s3_error!(InvalidArgument))?;

    let instant = time::OffsetDateTime::from(parsed);

    let milliseconds = instant
        .unix_timestamp()
        .checked_mul(1000)
        .ok_or_else(|| s3_error!(InvalidArgument))?;

    let expires =
        mikrio_core::EpochMillis::new(milliseconds).map_err(|_| s3_error!(InvalidArgument))?;

    Ok(Some(expires))
}

fn access_error(error: &AccessError) -> S3Error {
    match error {
        AccessError::Denied => s3_error!(AccessDenied),
        AccessError::Invalid(_) | AccessError::Identity(_) => s3_error!(InvalidArgument),
        _ => s3_error!(InternalError),
    }
}

pub(super) fn ambiguous_headers(headers: &http::HeaderMap) -> bool {
    const SINGLETONS: &[&str] = &[
        "host",
        "origin",
        "access-control-request-method",
        "access-control-request-headers",
        "authorization",
        "content-length",
        "transfer-encoding",
        "x-amz-date",
        "x-amz-content-sha256",
        "x-amz-security-token",
        "x-amz-expected-bucket-owner",
        "x-amz-decoded-content-length",
        "x-amz-trailer",
        "content-md5",
        "x-amz-sdk-checksum-algorithm",
        "x-amz-tagging",
    ];

    if SINGLETONS
        .iter()
        .any(|name| headers.get_all(*name).iter().count() > 1)
    {
        return true;
    }

    if headers.contains_key("content-length") && headers.contains_key("transfer-encoding") {
        return true;
    }

    headers.keys().any(|name| {
        name.as_str().starts_with("x-amz-checksum-") && headers.get_all(name).iter().count() > 1
    })
}

#[cfg(test)]
impl Access {
    async fn authorize_post_observation(
        &self,
        request: &s3s::S3Request<s3s::dto::PostObjectInput>,
    ) -> S3Result<()> {
        let identity = request
            .extensions
            .get::<Arc<HttpIdentity>>()
            .ok_or_else(|| s3_error!(AccessDenied))?;

        let context = Context::default();

        let check = AccessRequest {
            bucket: &request.input.bucket,
            expected_bucket_id: None,
            key: Some(&request.input.key),
            action: Action::parse("s3:PutObject").map_err(|_| s3_error!(InternalError))?,
            context: &context,
            expected_owner: None,
        }
        .into_owned()
        .map_err(|_| s3_error!(InvalidArgument))?;

        self.connection
            .authorize_http(Arc::clone(identity), vec![check])
            .await
            .map_err(|error| access_error(&error))
    }
}
