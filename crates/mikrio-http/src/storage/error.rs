//! Only controlled classifications cross the wire; native causes stay private.

use std::error::Error;
use std::fmt;
use std::io;

use mikrio_core::integrity::IntegrityError;
use mikrio_core::{OperationError, OperationFailure};
use s3s::{S3Error, S3ErrorCode, s3_error};

#[derive(Debug)]
pub(super) struct UploadFailure(pub(super) S3ErrorCode);

impl fmt::Display for UploadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HTTP upload validation failed")
    }
}

impl Error for UploadFailure {}

pub(super) fn upload(error: &S3Error) -> io::Error {
    io::Error::other(UploadFailure(error.code().clone()))
}

pub(crate) fn operation(error: &OperationError) -> S3Error {
    if error.outcome() == Some(mikrio_core::MutationOutcome::Unknown) {
        return s3_error!(InternalError);
    }

    // These private markers originate only in this adapter. Never serialize
    // arbitrary source messages, paths, credentials or native error chains.
    let mut source = error.source();

    while let Some(cause) = source {
        if let Some(integrity) = cause.downcast_ref::<IntegrityError>() {
            return integrity_error(*integrity);
        }

        let upload = cause
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
            .and_then(|cause| cause.downcast_ref::<UploadFailure>());

        if let Some(upload) = upload {
            return S3Error::new(upload.0.clone());
        }

        source = cause.source();
    }

    if error.failure() == OperationFailure::NotModified {
        return error.cache_validators().map_or_else(
            || s3_error!(InternalError),
            |validators| super::output::not_modified(validators).unwrap_or_else(|error| error),
        );
    }

    let mut projected = failure(error.failure());

    if let Some((actual, requested)) = error.part_counts() {
        projected.set_part_counts(actual, requested);
    }

    projected
}

pub(super) fn failure(failure: OperationFailure) -> S3Error {
    match failure {
        OperationFailure::InvalidPartNumber => {
            let mut error = S3Error::new(S3ErrorCode::Custom("InvalidPartNumber".into()));
            error.set_status_code(http::StatusCode::RANGE_NOT_SATISFIABLE);
            error
        }
        OperationFailure::InvalidRange => s3_error!(InvalidRange),
        OperationFailure::InvalidArgument => s3_error!(InvalidArgument),
        OperationFailure::InvalidRequest => s3_error!(InvalidRequest),
        OperationFailure::NoSuchBucket => s3_error!(NoSuchBucket),
        OperationFailure::NoSuchBucketPolicy => s3_error!(NoSuchBucketPolicy),
        OperationFailure::NoSuchCorsConfiguration => s3_error!(NoSuchCORSConfiguration),
        OperationFailure::NoSuchPublicAccessBlockConfiguration => {
            let mut error = S3Error::new(S3ErrorCode::Custom(
                "NoSuchPublicAccessBlockConfiguration".into(),
            ));

            error.set_status_code(http::StatusCode::NOT_FOUND);
            error
        }
        OperationFailure::BucketAlreadyOwnedByYou => s3_error!(BucketAlreadyOwnedByYou),
        OperationFailure::BucketNotEmpty => s3_error!(BucketNotEmpty),
        OperationFailure::NoSuchUpload => s3_error!(NoSuchUpload),
        OperationFailure::OperationAborted => s3_error!(OperationAborted),
        OperationFailure::InvalidPart => s3_error!(InvalidPart),
        OperationFailure::InvalidPartOrder => s3_error!(InvalidPartOrder),
        OperationFailure::EntityTooSmall => s3_error!(EntityTooSmall),
        OperationFailure::NoSuchKey => s3_error!(NoSuchKey),
        OperationFailure::PreconditionFailed => s3_error!(PreconditionFailed),
        OperationFailure::BadDigest | OperationFailure::BodyReadFailed => s3_error!(BadDigest),
        OperationFailure::IncompleteBody => s3_error!(IncompleteBody),
        OperationFailure::EntityTooLarge => s3_error!(EntityTooLarge),
        OperationFailure::AccessDenied => s3_error!(AccessDenied),
        OperationFailure::StoreBusy => s3_error!(ServiceUnavailable),
        _ => s3_error!(InternalError),
    }
}

pub(super) fn integrity_error(error: IntegrityError) -> S3Error {
    match error {
        IntegrityError::UnsupportedAlgorithm => s3_error!(NotImplemented),
        IntegrityError::AlgorithmMismatch | IntegrityError::DigestMismatch => s3_error!(BadDigest),
        IntegrityError::InvalidChecksumType | IntegrityError::InvalidPartCount => {
            s3_error!(InvalidRequest)
        }
        IntegrityError::InvalidDigest => s3_error!(InvalidDigest),
        IntegrityError::LengthMismatch => s3_error!(IncompleteBody),
        IntegrityError::TooLarge => s3_error!(EntityTooLarge),
    }
}
