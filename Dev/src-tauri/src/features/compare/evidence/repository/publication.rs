//! Atomic durable publication of a verified Compare result.

use std::sync::Arc;

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::result::{SuccessfulComparePublication, SuccessfulCompareResult};
use super::super::model::verification::CompareVerificationTicket;
use super::super::persistence;
use super::{storage_error, CompareResultRepository};

impl CompareResultRepository {
    pub(crate) fn publish_successful_version(
        &self,
        verification: &CompareVerificationTicket,
        version: SuccessfulCompareResult,
    ) -> Result<SuccessfulComparePublication, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        store.validate_successful_publication(verification, &version.owner.identity)?;
        let (version, job_name) = version.into_version();
        let persisted = match &self.persistence {
            Some(persistence) => persistence
                .publish(store.persistence_generation, version, &job_name)
                .map_err(|error| {
                    storage_error("Cannot retain the successful Compare result", error)
                })?,
            None => persistence::PersistedPublication {
                generation: store.persistence_generation.checked_add(1).ok_or_else(|| {
                    CompareResultRepositoryError::Storage(
                        "The in-memory Compare-result generation is exhausted".to_string(),
                    )
                })?,
                version: Arc::new(version),
            },
        };
        store.commit_successful_publication(
            verification,
            persisted.version,
            job_name,
            persisted.generation,
        )
    }
}
