//! In-memory commit after durable publication succeeds.

use std::sync::Arc;

use crate::contracts::compare::CompareWorkspaceLookupDto;

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::execution::CompareExecutionState;
use super::super::model::result::{CompareResultVersion, SuccessfulComparePublication};
use super::super::model::scope::CompareScope;
use super::super::model::verification::CompareVerificationTicket;
use super::CompareResultStore;

impl CompareResultStore {
    pub(in crate::features::compare::evidence) fn commit_successful_publication(
        &mut self,
        verification: &CompareVerificationTicket,
        version: Arc<CompareResultVersion>,
        job_name: String,
        persistence_generation: u64,
    ) -> Result<SuccessfulComparePublication, CompareResultRepositoryError> {
        let identity = version.identity.clone();
        let scope = CompareScope::from_identity(&identity);
        self.job_names.insert(identity.job_id.clone(), job_name);
        self.retained_identities_by_id
            .insert(identity.result_id.clone(), identity.clone());
        self.cache_version(version);
        self.latest_by_scope.insert(scope.clone(), identity.clone());
        self.persistence_generation = persistence_generation;
        *self
            .execution_by_scope
            .get_mut(&scope)
            .expect("the current verification ticket must have execution state") =
            CompareExecutionState::Fresh {
                verification_epoch: verification.epoch,
                identity: identity.clone(),
            };
        let workspace = match self.exact_workspace_lookup(&identity)? {
            CompareWorkspaceLookupDto::Found { workspace } => *workspace,
            CompareWorkspaceLookupDto::Missing { .. } => {
                return Err(CompareResultRepositoryError::FreshResultWasNotRetained(
                    identity,
                ));
            }
        };
        Ok(SuccessfulComparePublication { workspace })
    }
}
