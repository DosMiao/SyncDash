//! Workspace, execution-status, identical-page, and latest-result queries.

use crate::contracts::compare::{
    CompareIdentity, CompareScopeExecutionStatusDto, CompareWorkspaceLookupDto,
    CompareWorkspaceSnapshotDto, IdenticalPage,
};

use super::super::model::error::CompareResultRepositoryError;
#[cfg(test)]
use super::super::model::result::RetainedCompareResult;
use super::super::model::scope::CompareScope;
use super::{CompareResultRepository, CompareWorkspaceJobState};

/// The largest identical page a single request may materialize.
const IDENTICAL_PAGE_LIMIT: usize = 2000;

impl CompareResultRepository {
    /// Keep the freshness lock through a short reservation edge, so a newer verification cannot
    /// invalidate the result between the check and reservation. The callback must not call back
    /// into this repository.
    pub(crate) fn with_fresh_execution_eligibility<T>(
        &self,
        identity: &CompareIdentity,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let store = self.store.lock().unwrap();
        store
            .ensure_execution_fresh(identity)
            .map_err(|error| error.to_string())?;
        operation()
    }

    #[cfg(test)]
    pub(crate) fn latest_for(
        &self,
        job_id: &str,
        target_index: usize,
        config_revision: &str,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        let scope = CompareScope::new(job_id, target_index, config_revision);
        let Some(identity) = store.latest_identity(&scope) else {
            return Ok(None);
        };
        self.load_exact_locked(&mut store, &identity)?.map_or_else(
            || {
                Err(CompareResultRepositoryError::DanglingLatestVersion(
                    identity,
                ))
            },
            |retained| Ok(Some(retained)),
        )
    }

    /// One page of the rows this exact result judged identical. The page size is capped here
    /// rather than at the caller, so no request can ask for the whole identical table at once.
    pub(crate) fn identical_page(
        &self,
        identity: &CompareIdentity,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<IdenticalPage, String> {
        let retained = self
            .require_exact(identity)
            .map_err(|error| error.to_string())?;
        let (total, rows) = syncdash::pipeline::compare::evidence::identical_page(
            retained.source(),
            retained.target(),
            retained.compare_options(),
            query,
            offset,
            limit.min(IDENTICAL_PAGE_LIMIT),
        );
        Ok(IdenticalPage { total, rows })
    }

    pub(crate) fn reconcile_exact_workspace(
        &self,
        identity: &CompareIdentity,
        job_state: CompareWorkspaceJobState,
    ) -> Result<CompareWorkspaceLookupDto, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        if let CompareWorkspaceJobState::Current { job_name } = &job_state {
            self.rebind_job_name_locked(&mut store, &identity.job_id, job_name)?;
        }
        self.load_exact_locked(&mut store, identity)?;
        store.reconcile_exact_workspace(identity, job_state)
    }

    pub(crate) fn restore_workspace(
        &self,
        job_id: &str,
        target_index: usize,
        config_revision: &str,
    ) -> Result<CompareWorkspaceLookupDto, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        let scope = CompareScope::new(job_id, target_index, config_revision);
        let Some(identity) = store.latest_identity(&scope) else {
            return Ok(CompareWorkspaceLookupDto::Missing {
                execution_status: store.execution_status(&scope),
            });
        };
        let retained = self.load_exact_locked(&mut store, &identity)?.ok_or(
            CompareResultRepositoryError::DanglingLatestVersion(identity),
        )?;
        Ok(CompareWorkspaceLookupDto::Found {
            workspace: Box::new(CompareWorkspaceSnapshotDto {
                plan: retained.plan(),
                execution_status: store.execution_status(&scope),
            }),
        })
    }

    pub(crate) fn execution_status(&self, scope: &CompareScope) -> CompareScopeExecutionStatusDto {
        let store = self.store.lock().unwrap();
        store.execution_status(scope)
    }

    #[cfg(test)]
    pub(crate) fn execution_status_for_identity(
        &self,
        identity: &CompareIdentity,
    ) -> CompareScopeExecutionStatusDto {
        self.execution_status(&CompareScope::from_identity(identity))
    }
}
