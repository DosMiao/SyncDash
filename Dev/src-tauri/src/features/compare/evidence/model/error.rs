//! Repository errors with exact execution and verification context.

use crate::contracts::compare::CompareIdentity;

use super::scope::CompareScope;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CompareResultRepositoryError {
    Storage(String),
    DuplicateIdentity(CompareIdentity),
    IdentityMismatch {
        result_id: String,
    },
    MissingJobDisplayName {
        job_id: String,
    },
    DanglingLatestVersion(CompareIdentity),
    NotRetained,
    AwaitingSuccessfulCompare(CompareScope),
    ResultIsNotExecutionFresh {
        requested_run_id: u64,
        fresh_run_id: u64,
    },
    FreshResultWasNotRetained(CompareIdentity),
    VerificationEpochExhausted(CompareScope),
    VerificationScopeMismatch,
    VerificationWasSuperseded {
        submitted_epoch: u64,
        active_epoch: u64,
    },
    VerificationHasNotLaunched(CompareScope),
    VerificationRunMismatch {
        launched_run_id: u64,
        published_run_id: u64,
    },
    VerificationAlreadyPublished(CompareIdentity),
    VerificationIsNotActive(CompareIdentity),
}

impl std::fmt::Display for CompareResultRepositoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(message) => formatter.write_str(message),
            Self::DuplicateIdentity(_) => {
                formatter.write_str("This exact Compare result identity is already retained")
            }
            Self::IdentityMismatch { result_id } => write!(
                formatter,
                "Compare result ID '{result_id}' belongs to a different immutable identity"
            ),
            Self::MissingJobDisplayName { job_id } => write!(
                formatter,
                "The retained Compare repository lost the presentation name for job '{job_id}'"
            ),
            Self::DanglingLatestVersion(identity) => write!(
                formatter,
                "The latest Compare pointer for run {} has no retained version",
                identity.compare_run_id
            ),
            Self::NotRetained => formatter
                .write_str("This exact Compare result is no longer retained — run Compare again"),
            Self::AwaitingSuccessfulCompare(scope) => write!(
                formatter,
                "A newer Compare or AutoScan verification started for job '{}' target {} and has not published a successful result — wait for it to succeed or run Compare again",
                scope.job_id,
                scope.target_index + 1
            ),
            Self::ResultIsNotExecutionFresh {
                requested_run_id,
                fresh_run_id,
            } => write!(
                formatter,
                "Compare run {} is retained for viewing but run {} is the execution-eligible result for this job target",
                requested_run_id, fresh_run_id
            ),
            Self::FreshResultWasNotRetained(identity) => write!(
                formatter,
                "Execution-eligible Compare run {} is no longer retained — run Compare again",
                identity.compare_run_id
            ),
            Self::VerificationEpochExhausted(scope) => write!(
                formatter,
                "The Compare verification sequence for job '{}' target {} is exhausted — restart SyncDash",
                scope.job_id,
                scope.target_index + 1
            ),
            Self::VerificationScopeMismatch => write!(
                formatter,
                "The successful Compare does not belong to its verification ticket's exact job revision and target"
            ),
            Self::VerificationWasSuperseded {
                submitted_epoch,
                active_epoch,
            } => write!(
                formatter,
                "Compare verification epoch {submitted_epoch} was superseded by epoch {active_epoch}"
            ),
            Self::VerificationHasNotLaunched(scope) => write!(
                formatter,
                "The Compare verification for job '{}' target {} has not launched a run",
                scope.job_id,
                scope.target_index + 1
            ),
            Self::VerificationRunMismatch {
                launched_run_id,
                published_run_id,
            } => write!(
                formatter,
                "Compare run {published_run_id} cannot publish a verification launched by run {launched_run_id}"
            ),
            Self::VerificationAlreadyPublished(identity) => write!(
                formatter,
                "The verification ticket already published execution-eligible Compare run {}",
                identity.compare_run_id
            ),
            Self::VerificationIsNotActive(identity) => write!(
                formatter,
                "The verification for Compare run {} already reached a terminal status",
                identity.compare_run_id
            ),
        }
    }
}

impl std::error::Error for CompareResultRepositoryError {}
