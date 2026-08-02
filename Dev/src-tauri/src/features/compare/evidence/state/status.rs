//! Projection of internal execution state into the wire status contract.

use crate::contracts::compare::{CompareOwner, CompareScopeExecutionStatusDto};

use super::super::model::execution::CompareExecutionState;
use super::super::model::scope::CompareScope;
use super::CompareResultStore;

impl CompareResultStore {
    pub(in crate::features::compare::evidence) fn execution_status(
        &self,
        scope: &CompareScope,
    ) -> CompareScopeExecutionStatusDto {
        let Some(state) = self.execution_by_scope.get(scope) else {
            return CompareScopeExecutionStatusDto::Unavailable { scope: scope.dto() };
        };
        let attempt = state.verification_attempt();
        let scope = scope.dto();
        match state {
            CompareExecutionState::AwaitingCompare { .. } => {
                CompareScopeExecutionStatusDto::AwaitingCompare { scope, attempt }
            }
            CompareExecutionState::Comparing { .. } => {
                CompareScopeExecutionStatusDto::Comparing { scope, attempt }
            }
            CompareExecutionState::Fresh { identity, .. } => {
                CompareScopeExecutionStatusDto::Fresh {
                    scope,
                    attempt,
                    owner: CompareOwner {
                        identity: identity.clone(),
                        job_name: self
                            .job_names
                            .get(&identity.job_id)
                            .cloned()
                            .unwrap_or_else(|| {
                                panic!(
                                "execution-fresh Compare run {} has no retained presentation name",
                                identity.compare_run_id
                            )
                            }),
                    },
                }
            }
            CompareExecutionState::Failed { message, .. } => {
                CompareScopeExecutionStatusDto::Failed {
                    scope,
                    attempt,
                    message: message.clone(),
                }
            }
            CompareExecutionState::Cancelled { .. } => {
                CompareScopeExecutionStatusDto::Cancelled { scope, attempt }
            }
            CompareExecutionState::Expired { reason, .. } => {
                CompareScopeExecutionStatusDto::Expired {
                    scope,
                    attempt,
                    reason: *reason,
                }
            }
        }
    }
}
