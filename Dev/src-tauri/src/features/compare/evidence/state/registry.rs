//! Registry mutation effects over retained and executable state.

use crate::contracts::compare::{CompareExecutionExpiryReasonDto, CompareIdentity};

use super::super::model::execution::CompareExecutionState;
use super::super::model::scope::CompareScope;
use super::CompareResultStore;

impl CompareResultStore {
    pub(in crate::features::compare::evidence) fn expire_revision(
        &mut self,
        job_id: &str,
        config_revision: &str,
        reason: CompareExecutionExpiryReasonDto,
    ) {
        let scopes = self
            .execution_by_scope
            .keys()
            .filter(|scope| scope.job_id == job_id && scope.config_revision == config_revision)
            .cloned()
            .collect::<Vec<_>>();
        for scope in scopes {
            self.expire_scope(&scope, reason);
        }
    }

    pub(in crate::features::compare::evidence) fn expire_job(&mut self, job_id: &str) {
        let scopes = self
            .execution_by_scope
            .keys()
            .filter(|scope| scope.job_id == job_id)
            .cloned()
            .collect::<Vec<_>>();
        for scope in scopes {
            self.expire_scope(&scope, CompareExecutionExpiryReasonDto::JobDeleted);
        }
    }

    pub(in crate::features::compare::evidence) fn rebind_job_name(
        &mut self,
        job_id: &str,
        job_name: &str,
    ) {
        if self
            .retained_identities_by_id
            .values()
            .any(|identity| identity.job_id == job_id)
        {
            self.job_names
                .insert(job_id.to_string(), job_name.to_string());
        }
    }

    pub(in crate::features::compare::evidence) fn forget(
        &mut self,
        identity: &CompareIdentity,
        persistence_generation: u64,
    ) {
        self.retained_identities_by_id.remove(&identity.result_id);
        self.versions_by_id.remove(&identity.result_id);
        self.cache_order
            .retain(|cached| cached != &identity.result_id);
        let scope = CompareScope::from_identity(identity);
        if self.latest_by_scope.get(&scope) == Some(identity) {
            self.latest_by_scope.remove(&scope);
        }
        if matches!(
            self.execution_by_scope.get(&scope),
            Some(CompareExecutionState::Fresh { identity: fresh, .. }) if fresh == identity
        ) {
            self.execution_by_scope.remove(&scope);
        }
        self.job_names.retain(|job_id, _| {
            self.retained_identities_by_id
                .values()
                .any(|identity| identity.job_id == *job_id)
        });
        self.persistence_generation = persistence_generation;
    }
}
