//! Owned bucket configuration values shared by native and authenticated commands.

use std::fmt;

use crate::object::BucketName;

/// A live bucket name and an optional owner assertion; never an access grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketAccessRequest {
    pub bucket: BucketName,
    pub expected_owner: Option<String>,
}

/// Exact persisted policy document. Diagnostic output never includes its contents.
pub struct GetBucketPolicyOutput {
    pub document: String,
}

impl fmt::Debug for GetBucketPolicyOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GetBucketPolicyOutput")
            .finish_non_exhaustive()
    }
}
