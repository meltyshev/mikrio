//! Detached, bounded error values; no thread-local last-error state.

use std::error::Error as _;

use mikrio::MutationOutcome;
use mikrio_embed::{HostError, body::BodyError};

#[derive(Clone, Debug)]
pub(crate) struct Error {
    pub code: String,
    pub message: String,
    pub cause: Option<String>,
    pub outcome: Option<MutationOutcome>,
    pub partial_result: Option<mikrio::DeleteObjectsProgress>,
}

impl Error {
    pub(crate) fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: bounded(message),
            cause: None,
            outcome: None,
            partial_result: None,
        }
    }

    pub(crate) fn invalid(message: &str) -> Self {
        Self::new("InvalidArgument", message)
    }

    pub(crate) fn mutation(mut self, fallback: MutationOutcome) -> Self {
        self.outcome.get_or_insert(fallback);

        self
    }
}

impl From<&mikrio::StorageError> for Error {
    fn from(error: &mikrio::StorageError) -> Self {
        Self {
            code: error.code().as_str().into(),
            message: bounded(&error.to_string()),
            cause: error.source().map(|cause| bounded(&cause.to_string())),
            outcome: error.mutation_outcome().copied(),
            partial_result: error.partial_result().cloned(),
        }
    }
}

impl From<mikrio::StorageError> for Error {
    fn from(error: mikrio::StorageError) -> Self {
        Self::from(&error)
    }
}

impl From<HostError> for Error {
    fn from(error: HostError) -> Self {
        Self::from(&error)
    }
}

impl From<&HostError> for Error {
    fn from(error: &HostError) -> Self {
        let code = match error {
            HostError::Busy => "StoreBusy",
            HostError::InheritedProcess => "InvalidArgument",
            HostError::Unavailable(_) | HostError::Panicked => "IoError",
        };

        Self::new(code, &error.to_string())
    }
}

impl From<BodyError> for Error {
    fn from(error: BodyError) -> Self {
        if let BodyError::Native(native) = &error {
            return Self::from(native);
        }

        Self::new(error.code(), &error.to_string())
    }
}

fn bounded(value: &str) -> String {
    value.chars().take(1024).collect()
}
