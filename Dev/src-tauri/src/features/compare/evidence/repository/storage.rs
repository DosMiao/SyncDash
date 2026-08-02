//! Locked cache misses and durable presentation updates.

use crate::contracts::compare::CompareIdentity;

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::result::RetainedCompareResult;
use super::super::state::CompareResultStore;
use super::{storage_error, CompareResultRepository};

impl CompareResultRepository {
    pub(super) fn load_exact_locked(
        &self,
        store: &mut CompareResultStore,
        identity: &CompareIdentity,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        if !store.exact_identity_is_retained(identity)? {
            return Ok(None);
        }
        if let Some(retained) = store.cached_exact(identity)? {
            return Ok(Some(retained));
        }
        let Some(persistence) = &self.persistence else {
            return Err(CompareResultRepositoryError::FreshResultWasNotRetained(
                identity.clone(),
            ));
        };
        let version = persistence
            .load_exact(store.persistence_generation, identity)
            .map_err(|error| storage_error("Cannot load the exact Compare result", error))?;
        store.cache_version(version.clone());
        store.retained_result(version).map(Some)
    }

    pub(super) fn rebind_job_name_locked(
        &self,
        store: &mut CompareResultStore,
        job_id: &str,
        job_name: &str,
    ) -> Result<(), CompareResultRepositoryError> {
        if let Some(persistence) = &self.persistence {
            if let Some(generation) = persistence
                .rebind_job_name(store.persistence_generation, job_id, job_name)
                .map_err(|error| {
                    storage_error("Cannot update retained Compare presentation", error)
                })?
            {
                store.persistence_generation = generation;
            }
        }
        store.rebind_job_name(job_id, job_name);
        Ok(())
    }
}
