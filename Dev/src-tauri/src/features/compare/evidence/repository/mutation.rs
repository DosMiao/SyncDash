//! Registry-driven expiry, presentation rebinding, and explicit forgetting.

use crate::contracts::compare::{
    CompareExecutionExpiryReasonDto, CompareIdentity, CompareScopeExecutionStatusDto,
};

use super::super::model::error::CompareResultRepositoryError;
use super::super::persistence;
use super::{storage_error, CompareResultForgetOutcome, CompareResultRepository};

impl CompareResultRepository {
    pub(crate) fn expire_revision(
        &self,
        job_id: &str,
        config_revision: &str,
        reason: CompareExecutionExpiryReasonDto,
    ) -> Vec<CompareScopeExecutionStatusDto> {
        let mut store = self.store.lock().unwrap();
        store.expire_revision(job_id, config_revision, reason);
        store
            .execution_by_scope
            .keys()
            .filter(|scope| scope.job_id == job_id && scope.config_revision == config_revision)
            .map(|scope| store.execution_status(scope))
            .collect()
    }

    pub(crate) fn expire_job(&self, job_id: &str) -> Vec<CompareScopeExecutionStatusDto> {
        let mut store = self.store.lock().unwrap();
        store.expire_job(job_id);
        store
            .execution_by_scope
            .keys()
            .filter(|scope| scope.job_id == job_id)
            .map(|scope| store.execution_status(scope))
            .collect()
    }

    pub(crate) fn rebind_job_name(
        &self,
        job_id: &str,
        job_name: &str,
    ) -> Result<(), CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        self.rebind_job_name_locked(&mut store, job_id, job_name)
    }

    pub(crate) fn forget(
        &self,
        identity: &CompareIdentity,
    ) -> Result<CompareResultForgetOutcome, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        if !store.exact_identity_is_retained(identity)? {
            return Ok(CompareResultForgetOutcome::AlreadyForgotten);
        }
        let outcome = match &self.persistence {
            Some(persistence) => persistence
                .forget(store.persistence_generation, identity)
                .map_err(|error| storage_error("Cannot forget the Compare result", error))?,
            None => persistence::PersistedForget {
                generation: store.persistence_generation.checked_add(1).ok_or_else(|| {
                    CompareResultRepositoryError::Storage(
                        "The in-memory Compare-result generation is exhausted".to_string(),
                    )
                })?,
                cleanup_error: None,
            },
        };
        store.forget(identity, outcome.generation);
        Ok(CompareResultForgetOutcome::Forgotten {
            cleanup_warning: outcome.cleanup_error,
        })
    }
}
