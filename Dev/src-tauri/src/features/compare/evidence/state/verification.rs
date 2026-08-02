//! Verification epoch, launch, terminal, and freshness transitions.

use crate::contracts::compare::{CompareExecutionExpiryReasonDto, CompareIdentity};

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::execution::CompareExecutionState;
use super::super::model::scope::CompareScope;
use super::super::model::verification::{
    CompareVerificationTerminalOutcome, CompareVerificationTicket,
};
use super::CompareResultStore;

impl CompareResultStore {
    pub(in crate::features::compare::evidence) fn begin_verification(
        &mut self,
        scope: CompareScope,
        launched_run_id: Option<u64>,
    ) -> Result<CompareVerificationTicket, CompareResultRepositoryError> {
        let epoch = match self.execution_by_scope.get_mut(&scope) {
            Some(state) => {
                let Some(epoch) = state.verification_epoch().checked_add(1) else {
                    state.expire(CompareExecutionExpiryReasonDto::VerificationExhausted);
                    return Err(CompareResultRepositoryError::VerificationEpochExhausted(
                        scope,
                    ));
                };
                epoch
            }
            None => 1,
        };
        let state = match launched_run_id {
            Some(launched_run_id) => CompareExecutionState::Comparing {
                verification_epoch: epoch,
                launched_run_id,
            },
            None => CompareExecutionState::AwaitingCompare {
                verification_epoch: epoch,
            },
        };
        self.execution_by_scope.insert(scope.clone(), state);
        Ok(CompareVerificationTicket { scope, epoch })
    }

    pub(in crate::features::compare::evidence) fn mark_verification_comparing(
        &mut self,
        verification: &CompareVerificationTicket,
        launched_run_id: u64,
    ) -> bool {
        let Some(state) = self.execution_by_scope.get_mut(&verification.scope) else {
            return false;
        };
        match state {
            CompareExecutionState::AwaitingCompare { verification_epoch }
                if *verification_epoch == verification.epoch =>
            {
                *state = CompareExecutionState::Comparing {
                    verification_epoch: verification.epoch,
                    launched_run_id,
                };
                true
            }
            _ => false,
        }
    }

    pub(in crate::features::compare::evidence) fn complete_verification_terminal(
        &mut self,
        verification: &CompareVerificationTicket,
        outcome: CompareVerificationTerminalOutcome,
    ) -> bool {
        let Some(state) = self.execution_by_scope.get_mut(&verification.scope) else {
            return false;
        };
        let launched_run_id = match state {
            CompareExecutionState::AwaitingCompare { verification_epoch }
                if *verification_epoch == verification.epoch =>
            {
                None
            }
            CompareExecutionState::Comparing {
                verification_epoch,
                launched_run_id,
            } if *verification_epoch == verification.epoch => Some(*launched_run_id),
            _ => return false,
        };
        *state = match outcome {
            CompareVerificationTerminalOutcome::Failed { message } => {
                CompareExecutionState::Failed {
                    verification_epoch: verification.epoch,
                    launched_run_id,
                    message,
                }
            }
            CompareVerificationTerminalOutcome::Cancelled => CompareExecutionState::Cancelled {
                verification_epoch: verification.epoch,
                launched_run_id,
            },
        };
        true
    }

    pub(in crate::features::compare::evidence) fn ensure_execution_fresh(
        &self,
        identity: &CompareIdentity,
    ) -> Result<(), CompareResultRepositoryError> {
        let scope = CompareScope::from_identity(identity);
        match self.execution_by_scope.get(&scope) {
            Some(CompareExecutionState::Fresh {
                identity: fresh, ..
            }) if fresh == identity => {
                if self.retained_identities_by_id.get(&identity.result_id) == Some(identity) {
                    Ok(())
                } else {
                    Err(CompareResultRepositoryError::FreshResultWasNotRetained(
                        identity.clone(),
                    ))
                }
            }
            Some(CompareExecutionState::Fresh {
                identity: fresh, ..
            }) => Err(CompareResultRepositoryError::ResultIsNotExecutionFresh {
                requested_run_id: identity.compare_run_id,
                fresh_run_id: fresh.compare_run_id,
            }),
            Some(
                CompareExecutionState::AwaitingCompare { .. }
                | CompareExecutionState::Comparing { .. }
                | CompareExecutionState::Failed { .. }
                | CompareExecutionState::Cancelled { .. }
                | CompareExecutionState::Expired { .. },
            )
            | None => Err(CompareResultRepositoryError::AwaitingSuccessfulCompare(
                scope,
            )),
        }
    }
}
