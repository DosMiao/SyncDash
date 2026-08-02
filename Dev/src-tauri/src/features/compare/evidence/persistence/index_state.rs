//! Copy-on-write authoritative index transitions.

use std::collections::BTreeMap;

use crate::contracts::compare::CompareIdentity;

use super::super::model::scope::CompareScope;
use super::error::invalid_data;
use super::index_order::compare_latest_scope;
use super::index_validation::validate_index_state;
use super::schema::{IndexState, IndexedResult, LatestResult};

impl IndexState {
    pub(super) fn empty() -> Self {
        Self {
            generation: 0,
            last_publication_sequence: 0,
            results: BTreeMap::new(),
            latest_by_scope: Vec::new(),
            job_names: BTreeMap::new(),
        }
    }

    fn next_generation(&self) -> std::io::Result<u64> {
        self.generation
            .checked_add(1)
            .ok_or_else(|| invalid_data("the Compare-result repository generation is exhausted"))
    }

    pub(super) fn publish(
        &self,
        identity: &CompareIdentity,
        artifact_checksum: String,
        job_name: &str,
    ) -> std::io::Result<Self> {
        if self.results.contains_key(&identity.result_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Compare result '{}' already exists", identity.result_id),
            ));
        }
        let publication_sequence = self
            .last_publication_sequence
            .checked_add(1)
            .ok_or_else(|| invalid_data("the Compare-result publication sequence is exhausted"))?;
        let mut next = self.clone();
        next.generation = self.next_generation()?;
        next.last_publication_sequence = publication_sequence;
        next.results.insert(
            identity.result_id.clone(),
            IndexedResult {
                identity: identity.clone(),
                publication_sequence,
                artifact_checksum,
            },
        );
        next.job_names
            .insert(identity.job_id.clone(), job_name.to_string());
        let scope = CompareScope::from_identity(identity);
        next.latest_by_scope
            .retain(|latest| !latest.matches(&scope));
        next.latest_by_scope
            .push(LatestResult::new(&scope, identity.result_id.clone()));
        next.latest_by_scope.sort_by(compare_latest_scope);
        validate_index_state(&next)?;
        Ok(next)
    }

    pub(super) fn rebind_job_name(
        &self,
        job_id: &str,
        job_name: &str,
    ) -> std::io::Result<Option<Self>> {
        let Some(current_name) = self.job_names.get(job_id) else {
            return Ok(None);
        };
        if current_name == job_name {
            return Ok(None);
        }
        let mut next = self.clone();
        next.generation = self.next_generation()?;
        next.job_names
            .insert(job_id.to_string(), job_name.to_string());
        validate_index_state(&next)?;
        Ok(Some(next))
    }

    pub(super) fn forget(&self, identity: &CompareIdentity) -> std::io::Result<Self> {
        let indexed = self.results.get(&identity.result_id).ok_or_else(|| {
            invalid_data(format!(
                "Compare result '{}' is missing from the authoritative index",
                identity.result_id
            ))
        })?;
        if indexed.identity != *identity {
            return Err(invalid_data(format!(
                "Compare result ID '{}' belongs to a different immutable identity",
                identity.result_id
            )));
        }
        let mut next = self.clone();
        next.generation = self.next_generation()?;
        next.results.remove(&identity.result_id);
        next.latest_by_scope
            .retain(|latest| latest.result_id != identity.result_id);
        if !next
            .results
            .values()
            .any(|result| result.identity.job_id == identity.job_id)
        {
            next.job_names.remove(&identity.job_id);
        }
        validate_index_state(&next)?;
        Ok(next)
    }
}

impl LatestResult {
    fn new(scope: &CompareScope, result_id: String) -> Self {
        Self {
            job_id: scope.job_id.clone(),
            target_index: scope.target_index,
            config_revision: scope.config_revision.clone(),
            result_id,
        }
    }

    pub(super) fn scope(&self) -> CompareScope {
        CompareScope::new(&self.job_id, self.target_index, &self.config_revision)
    }

    fn matches(&self, scope: &CompareScope) -> bool {
        self.job_id == scope.job_id
            && self.target_index == scope.target_index
            && self.config_revision == scope.config_revision
    }
}
