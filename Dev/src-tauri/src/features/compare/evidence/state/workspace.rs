//! Atomic workspace restoration and reconciliation.

use crate::contracts::compare::{
    CompareExecutionExpiryReasonDto, CompareIdentity, CompareWorkspaceLookupDto,
    CompareWorkspaceSnapshotDto,
};

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::scope::CompareScope;
use super::super::repository::CompareWorkspaceJobState;
use super::CompareResultStore;

impl CompareResultStore {
    pub(in crate::features::compare::evidence) fn exact_workspace_lookup(
        &mut self,
        identity: &CompareIdentity,
    ) -> Result<CompareWorkspaceLookupDto, CompareResultRepositoryError> {
        let scope = CompareScope::from_identity(identity);
        let Some(retained) = self.cached_exact(identity)? else {
            return Ok(CompareWorkspaceLookupDto::Missing {
                execution_status: self.execution_status(&scope),
            });
        };
        Ok(CompareWorkspaceLookupDto::Found {
            workspace: Box::new(CompareWorkspaceSnapshotDto {
                plan: retained.plan(),
                execution_status: self.execution_status(&scope),
            }),
        })
    }

    pub(in crate::features::compare::evidence) fn reconcile_exact_workspace(
        &mut self,
        identity: &CompareIdentity,
        job_state: CompareWorkspaceJobState,
    ) -> Result<CompareWorkspaceLookupDto, CompareResultRepositoryError> {
        let scope = CompareScope::from_identity(identity);
        match job_state {
            CompareWorkspaceJobState::Current { job_name } => {
                self.rebind_job_name(&identity.job_id, &job_name);
            }
            CompareWorkspaceJobState::ConfigurationChanged => {
                self.expire_scope(&scope, CompareExecutionExpiryReasonDto::JobChanged);
            }
            CompareWorkspaceJobState::Deleted => {
                self.expire_scope(&scope, CompareExecutionExpiryReasonDto::JobDeleted);
            }
        }
        self.exact_workspace_lookup(identity)
    }

    pub(in crate::features::compare::evidence) fn latest_identity(
        &self,
        scope: &CompareScope,
    ) -> Option<CompareIdentity> {
        self.latest_by_scope.get(scope).cloned()
    }
}
