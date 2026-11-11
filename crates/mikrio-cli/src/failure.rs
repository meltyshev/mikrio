//! Redacted command-boundary failures and authoritative mutation outcomes.

use mikrio::{ErrorCode, MutationOutcome, StorageError};
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Phase {
    Arguments,
    Input,
    Open,
    Operation,
    Output,
    Close,
}

pub(crate) struct Failure {
    pub(crate) phase: Phase,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) outcome: Option<MutationOutcome>,
    pub(crate) exit: u8,
}

impl Failure {
    pub(crate) fn argument(message: impl Into<String>) -> Self {
        Self {
            phase: Phase::Arguments,
            code: ErrorCode::InvalidArgument.as_str(),
            message: message.into(),
            outcome: None,
            exit: 2,
        }
    }

    pub(crate) fn io(phase: Phase, message: &'static str) -> Self {
        Self {
            phase,
            code: ErrorCode::IoError.as_str(),
            message: message.into(),
            outcome: None,
            exit: 1,
        }
    }

    pub(crate) fn storage(phase: Phase, error: &StorageError, policy: bool) -> Self {
        let invalid = matches!(phase, Phase::Arguments | Phase::Input)
            || (policy && error.code() == ErrorCode::InvalidArgument);

        Self {
            phase,
            code: error.code().as_str(),
            // The public facade's Display is redacted; do not traverse causes.
            message: error.to_string(),
            outcome: error.mutation_outcome().copied(),
            exit: if invalid { 2 } else { 1 },
        }
    }

    pub(crate) fn interrupted(phase: Phase) -> Self {
        Self {
            phase,
            code: ErrorCode::OperationCanceled.as_str(),
            message: "shutdown requested before the command completed".into(),
            outcome: None,
            exit: 1,
        }
    }

    pub(crate) fn completed(mut self, mutation: bool) -> Self {
        if mutation {
            self.outcome = Some(MutationOutcome::Committed);
            self.message
                .push_str("; the command committed, do not repeat creation automatically");
        }

        self
    }
}

impl From<StorageError> for Failure {
    fn from(error: StorageError) -> Self {
        Self::storage(Phase::Operation, &error, false)
    }
}
