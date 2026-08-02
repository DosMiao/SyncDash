//! Verification-attempt state transitions.

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::scope::CompareScope;
use super::super::model::verification::{
    CompareVerificationTerminalOutcome, CompareVerificationTicket,
};
use super::CompareResultRepository;

impl CompareResultRepository {
    pub(crate) fn begin_verification(
        &self,
        scope: CompareScope,
        launched_run_id: Option<u64>,
    ) -> Result<CompareVerificationTicket, CompareResultRepositoryError> {
        self.store
            .lock()
            .unwrap()
            .begin_verification(scope, launched_run_id)
    }

    pub(crate) fn mark_verification_comparing(
        &self,
        verification: &CompareVerificationTicket,
        launched_run_id: u64,
    ) -> bool {
        self.store
            .lock()
            .unwrap()
            .mark_verification_comparing(verification, launched_run_id)
    }

    pub(crate) fn complete_verification_terminal(
        &self,
        verification: &CompareVerificationTicket,
        outcome: CompareVerificationTerminalOutcome,
    ) -> bool {
        self.store
            .lock()
            .unwrap()
            .complete_verification_terminal(verification, outcome)
    }
}
