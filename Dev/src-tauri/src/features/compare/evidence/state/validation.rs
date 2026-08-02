//! Fail-closed validation of an immutable publication against its verification ticket.

use crate::contracts::compare::CompareIdentity;

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::execution::CompareExecutionState;
use super::super::model::scope::CompareScope;
use super::super::model::verification::CompareVerificationTicket;
use super::CompareResultStore;

impl CompareResultStore {
    pub(in crate::features::compare::evidence) fn validate_successful_publication(
        &self,
        verification: &CompareVerificationTicket,
        identity: &CompareIdentity,
    ) -> Result<(), CompareResultRepositoryError> {
        let scope = CompareScope::from_identity(identity);
        if verification.scope != scope {
            return Err(CompareResultRepositoryError::VerificationScopeMismatch);
        }
        let state = self
            .execution_by_scope
            .get(&verification.scope)
            .expect("a repository-issued verification ticket must retain execution state");
        if state.verification_epoch() != verification.epoch {
            return Err(CompareResultRepositoryError::VerificationWasSuperseded {
                submitted_epoch: verification.epoch,
                active_epoch: state.verification_epoch(),
            });
        }
        match state {
            CompareExecutionState::AwaitingCompare { .. } => {
                return Err(CompareResultRepositoryError::VerificationHasNotLaunched(
                    scope,
                ));
            }
            CompareExecutionState::Comparing {
                launched_run_id, ..
            } => {
                if *launched_run_id != identity.compare_run_id {
                    return Err(CompareResultRepositoryError::VerificationRunMismatch {
                        launched_run_id: *launched_run_id,
                        published_run_id: identity.compare_run_id,
                    });
                }
            }
            CompareExecutionState::Fresh {
                identity: published,
                ..
            } => {
                return Err(CompareResultRepositoryError::VerificationAlreadyPublished(
                    published.clone(),
                ));
            }
            CompareExecutionState::Failed { .. }
            | CompareExecutionState::Cancelled { .. }
            | CompareExecutionState::Expired { .. } => {
                return Err(CompareResultRepositoryError::VerificationIsNotActive(
                    identity.clone(),
                ));
            }
        }
        if self
            .retained_identities_by_id
            .contains_key(&identity.result_id)
        {
            return Err(CompareResultRepositoryError::DuplicateIdentity(
                identity.clone(),
            ));
        }
        Ok(())
    }
}
