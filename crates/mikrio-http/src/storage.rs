//! Authenticated object operations over the core's shared storage authority.

mod attributes;
mod body;
mod buckets;
mod configuration;
mod copy;
mod cors;
mod deletion;
pub(super) mod error;
mod input;
mod integrity;
mod lifecycle;
mod listing;
mod multipart;
mod output;
mod post;
mod progress;
mod tagging;

use std::sync::Arc;

use mikrio_core::Connection;
use mikrio_core::access::HttpIdentity;
use s3s::dto::{
    DeleteObjectInput, DeleteObjectOutput, GetObjectInput, GetObjectOutput, HeadObjectInput,
    HeadObjectOutput, PutObjectInput, PutObjectOutput,
};
use s3s::{S3Request, S3Response, S3Result, s3_error};

pub(super) struct Storage(pub(super) Connection);

fn identity<T>(request: &S3Request<T>) -> S3Result<Arc<HttpIdentity>> {
    request
        .extensions
        .get::<Arc<HttpIdentity>>()
        .cloned()
        .ok_or_else(|| s3_error!(AccessDenied))
}

#[async_trait::async_trait]
impl s3s::S3 for Storage {
    async fn get_object_attributes(
        &self,
        request: S3Request<s3s::dto::GetObjectAttributesInput>,
    ) -> S3Result<S3Response<s3s::dto::GetObjectAttributesOutput>> {
        attributes::get(self, request).await
    }

    async fn create_multipart_upload(
        &self,
        request: S3Request<s3s::dto::CreateMultipartUploadInput>,
    ) -> S3Result<S3Response<s3s::dto::CreateMultipartUploadOutput>> {
        multipart::create(self, request).await
    }

    async fn upload_part(
        &self,
        request: S3Request<s3s::dto::UploadPartInput>,
    ) -> S3Result<S3Response<s3s::dto::UploadPartOutput>> {
        multipart::part(self, request).await
    }

    async fn upload_part_copy(
        &self,
        request: S3Request<s3s::dto::UploadPartCopyInput>,
    ) -> S3Result<S3Response<s3s::dto::UploadPartCopyOutput>> {
        multipart::copy(self, request).await
    }

    async fn complete_multipart_upload(
        &self,
        request: S3Request<s3s::dto::CompleteMultipartUploadInput>,
    ) -> S3Result<S3Response<s3s::dto::CompleteMultipartUploadOutput>> {
        multipart::complete(self, request).await
    }

    async fn abort_multipart_upload(
        &self,
        request: S3Request<s3s::dto::AbortMultipartUploadInput>,
    ) -> S3Result<S3Response<s3s::dto::AbortMultipartUploadOutput>> {
        multipart::abort(self, request).await
    }

    async fn list_parts(
        &self,
        request: S3Request<s3s::dto::ListPartsInput>,
    ) -> S3Result<S3Response<s3s::dto::ListPartsOutput>> {
        multipart::parts(self, request).await
    }

    async fn list_multipart_uploads(
        &self,
        request: S3Request<s3s::dto::ListMultipartUploadsInput>,
    ) -> S3Result<S3Response<s3s::dto::ListMultipartUploadsOutput>> {
        multipart::uploads(self, request).await
    }

    async fn copy_object(
        &self,
        request: S3Request<s3s::dto::CopyObjectInput>,
    ) -> S3Result<S3Response<s3s::dto::CopyObjectOutput>> {
        copy::copy(self, request).await
    }

    async fn get_object_tagging(
        &self,
        request: S3Request<s3s::dto::GetObjectTaggingInput>,
    ) -> S3Result<S3Response<s3s::dto::GetObjectTaggingOutput>> {
        tagging::get(self, request).await
    }

    async fn put_object_tagging(
        &self,
        request: S3Request<s3s::dto::PutObjectTaggingInput>,
    ) -> S3Result<S3Response<s3s::dto::PutObjectTaggingOutput>> {
        tagging::put(self, request).await
    }

    async fn delete_object_tagging(
        &self,
        request: S3Request<s3s::dto::DeleteObjectTaggingInput>,
    ) -> S3Result<S3Response<s3s::dto::DeleteObjectTaggingOutput>> {
        tagging::delete(self, request).await
    }

    async fn list_buckets(
        &self,
        request: S3Request<s3s::dto::ListBucketsInput>,
    ) -> S3Result<S3Response<s3s::dto::ListBucketsOutput>> {
        listing::buckets(self, request).await
    }

    async fn list_objects_v2(
        &self,
        request: S3Request<s3s::dto::ListObjectsV2Input>,
    ) -> S3Result<S3Response<s3s::dto::ListObjectsV2Output>> {
        listing::objects(self, request).await
    }

    async fn put_bucket_cors(
        &self,
        request: S3Request<s3s::dto::PutBucketCorsInput>,
    ) -> S3Result<S3Response<s3s::dto::PutBucketCorsOutput>> {
        cors::put(self, request).await
    }

    async fn get_bucket_cors(
        &self,
        request: S3Request<s3s::dto::GetBucketCorsInput>,
    ) -> S3Result<S3Response<s3s::dto::GetBucketCorsOutput>> {
        cors::get(self, request).await
    }

    async fn delete_bucket_cors(
        &self,
        request: S3Request<s3s::dto::DeleteBucketCorsInput>,
    ) -> S3Result<S3Response<s3s::dto::DeleteBucketCorsOutput>> {
        cors::delete(self, request).await
    }

    async fn put_bucket_lifecycle_configuration(
        &self,
        request: S3Request<s3s::dto::PutBucketLifecycleConfigurationInput>,
    ) -> S3Result<S3Response<s3s::dto::PutBucketLifecycleConfigurationOutput>> {
        lifecycle::put(self, request).await
    }

    async fn get_bucket_lifecycle_configuration(
        &self,
        request: S3Request<s3s::dto::GetBucketLifecycleConfigurationInput>,
    ) -> S3Result<S3Response<s3s::dto::GetBucketLifecycleConfigurationOutput>> {
        lifecycle::get(self, request).await
    }

    async fn delete_bucket_lifecycle(
        &self,
        request: S3Request<s3s::dto::DeleteBucketLifecycleInput>,
    ) -> S3Result<S3Response<s3s::dto::DeleteBucketLifecycleOutput>> {
        lifecycle::delete(self, request).await
    }

    async fn put_bucket_policy(
        &self,
        request: S3Request<s3s::dto::PutBucketPolicyInput>,
    ) -> S3Result<S3Response<s3s::dto::PutBucketPolicyOutput>> {
        configuration::put_policy(self, request).await
    }

    async fn get_bucket_policy(
        &self,
        request: S3Request<s3s::dto::GetBucketPolicyInput>,
    ) -> S3Result<S3Response<s3s::dto::GetBucketPolicyOutput>> {
        configuration::get_policy(self, request).await
    }

    async fn delete_bucket_policy(
        &self,
        request: S3Request<s3s::dto::DeleteBucketPolicyInput>,
    ) -> S3Result<S3Response<s3s::dto::DeleteBucketPolicyOutput>> {
        configuration::delete_policy(self, request).await
    }

    async fn get_bucket_policy_status(
        &self,
        request: S3Request<s3s::dto::GetBucketPolicyStatusInput>,
    ) -> S3Result<S3Response<s3s::dto::GetBucketPolicyStatusOutput>> {
        configuration::policy_status(self, request).await
    }

    async fn put_public_access_block(
        &self,
        request: S3Request<s3s::dto::PutPublicAccessBlockInput>,
    ) -> S3Result<S3Response<s3s::dto::PutPublicAccessBlockOutput>> {
        configuration::put_block(self, request).await
    }

    async fn get_public_access_block(
        &self,
        request: S3Request<s3s::dto::GetPublicAccessBlockInput>,
    ) -> S3Result<S3Response<s3s::dto::GetPublicAccessBlockOutput>> {
        configuration::get_block(self, request).await
    }

    async fn delete_public_access_block(
        &self,
        request: S3Request<s3s::dto::DeletePublicAccessBlockInput>,
    ) -> S3Result<S3Response<s3s::dto::DeletePublicAccessBlockOutput>> {
        configuration::delete_block(self, request).await
    }

    async fn put_bucket_ownership_controls(
        &self,
        request: S3Request<s3s::dto::PutBucketOwnershipControlsInput>,
    ) -> S3Result<S3Response<s3s::dto::PutBucketOwnershipControlsOutput>> {
        configuration::put_ownership(self, request).await
    }

    async fn get_bucket_ownership_controls(
        &self,
        request: S3Request<s3s::dto::GetBucketOwnershipControlsInput>,
    ) -> S3Result<S3Response<s3s::dto::GetBucketOwnershipControlsOutput>> {
        configuration::get_ownership(self, request).await
    }

    async fn get_bucket_acl(
        &self,
        request: S3Request<s3s::dto::GetBucketAclInput>,
    ) -> S3Result<S3Response<s3s::dto::GetBucketAclOutput>> {
        configuration::bucket_acl(self, request).await
    }

    async fn get_object_acl(
        &self,
        request: S3Request<s3s::dto::GetObjectAclInput>,
    ) -> S3Result<S3Response<s3s::dto::GetObjectAclOutput>> {
        configuration::object_acl(self, request).await
    }

    async fn create_bucket(
        &self,
        request: S3Request<s3s::dto::CreateBucketInput>,
    ) -> S3Result<S3Response<s3s::dto::CreateBucketOutput>> {
        buckets::create(self, request).await
    }

    async fn head_bucket(
        &self,
        request: S3Request<s3s::dto::HeadBucketInput>,
    ) -> S3Result<S3Response<s3s::dto::HeadBucketOutput>> {
        buckets::head(self, request).await
    }

    async fn get_bucket_location(
        &self,
        request: S3Request<s3s::dto::GetBucketLocationInput>,
    ) -> S3Result<S3Response<s3s::dto::GetBucketLocationOutput>> {
        buckets::location(self, request).await
    }

    async fn delete_bucket(
        &self,
        request: S3Request<s3s::dto::DeleteBucketInput>,
    ) -> S3Result<S3Response<s3s::dto::DeleteBucketOutput>> {
        buckets::delete(self, request).await
    }

    async fn post_object(
        &self,
        request: S3Request<s3s::dto::PostObjectInput>,
    ) -> S3Result<S3Response<s3s::dto::PostObjectOutput>> {
        post::post(self, request).await
    }

    async fn put_object(
        &self,
        mut request: S3Request<PutObjectInput>,
    ) -> S3Result<S3Response<PutObjectOutput>> {
        let identity = identity(&request)?;
        let integrity = integrity::Integrity::parse(&request)?;
        let put = input::put(&request, integrity.input.clone(), integrity.length)?;

        let body = request
            .input
            .body
            .take()
            .ok_or_else(|| s3_error!(InvalidRequest))?;

        let facts = self
            .0
            .put_object_http(identity, put, body::Upload::new(body), move || {
                integrity.finish()
            })
            .await
            .map_err(|error| error::operation(&error))?;

        Ok(S3Response::new(output::put(&facts)?))
    }

    async fn get_object(
        &self,
        request: S3Request<GetObjectInput>,
    ) -> S3Result<S3Response<GetObjectOutput>> {
        integrity::Integrity::bodyless(&request)?;

        let identity = identity(&request)?;
        let checksums = input::checksums(&request.headers)?;
        let read = input::read(&request)?;

        let (selected, body) = self
            .0
            .get_object_http(identity, read)
            .await
            .map_err(|error| error::operation(&error))?;

        let part_read = selected.is_part_read();
        let metadata = output::head(selected, checksums)?;

        let mut response = S3Response::new(output::get(metadata, body::download(body)));

        if part_read {
            response.status = Some(http::StatusCode::PARTIAL_CONTENT);
        }

        Ok(response)
    }

    async fn head_object(
        &self,
        request: S3Request<HeadObjectInput>,
    ) -> S3Result<S3Response<HeadObjectOutput>> {
        integrity::Integrity::bodyless(&request)?;

        let request = request.map_input(input::head);
        let identity = identity(&request)?;
        let checksums = input::checksums(&request.headers)?;
        let read = input::read(&request)?;

        let selected = self
            .0
            .head_object_http(identity, read)
            .await
            .map_err(|error| error::operation(&error))?;

        let part_read = selected.is_part_read();
        let mut response = S3Response::new(output::head(selected, checksums)?);

        if part_read {
            response.status = Some(http::StatusCode::PARTIAL_CONTENT);
        }

        Ok(response)
    }

    async fn delete_objects(
        &self,
        request: S3Request<s3s::dto::DeleteObjectsInput>,
    ) -> S3Result<S3Response<s3s::dto::DeleteObjectsOutput>> {
        deletion::delete(self, request).await
    }

    async fn delete_object(
        &self,
        request: S3Request<DeleteObjectInput>,
    ) -> S3Result<S3Response<DeleteObjectOutput>> {
        integrity::Integrity::bodyless(&request)?;
        let identity = identity(&request)?;
        let (read, condition) = input::delete(&request)?;

        // s3s validates the complete empty request envelope before this dispatch.
        self.0
            .delete_object_http(identity, read, condition)
            .await
            .map_err(|error| error::operation(&error))?;

        Ok(S3Response::new(DeleteObjectOutput::default()))
    }
}
