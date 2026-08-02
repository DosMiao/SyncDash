//! In-memory Compare execution state and bounded hot-cache transitions.

mod cache;
mod expiry;
mod publication;
mod registry;
mod status;
mod validation;
mod verification;
mod workspace;

use std::collections::HashMap;
use std::sync::Arc;

use crate::contracts::compare::{CompareExecutionExpiryReasonDto, CompareIdentity};

use super::model::execution::CompareExecutionState;
use super::model::result::CompareResultVersion;
use super::model::scope::CompareScope;
use super::persistence::LoadedCompareResults;

pub(super) const HOT_RESULT_CACHE_CAPACITY: usize = 4;

pub(super) struct CompareResultStore {
    pub(super) versions_by_id: HashMap<String, Arc<CompareResultVersion>>,
    pub(super) cache_order: std::collections::VecDeque<String>,
    pub(super) retained_identities_by_id: HashMap<String, CompareIdentity>,
    pub(super) cache_capacity: usize,
    pub(super) latest_by_scope: HashMap<CompareScope, CompareIdentity>,
    pub(super) execution_by_scope: HashMap<CompareScope, CompareExecutionState>,
    pub(super) job_names: HashMap<String, String>,
    pub(super) persistence_generation: u64,
}

impl CompareResultStore {
    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self {
            versions_by_id: HashMap::new(),
            cache_order: std::collections::VecDeque::new(),
            retained_identities_by_id: HashMap::new(),
            cache_capacity: usize::MAX,
            latest_by_scope: HashMap::new(),
            execution_by_scope: HashMap::new(),
            job_names: HashMap::new(),
            persistence_generation: 0,
        }
    }

    pub(super) fn from_loaded(loaded: LoadedCompareResults) -> Self {
        let execution_by_scope = loaded
            .latest_by_scope
            .iter()
            .map(|(scope, identity)| {
                (
                    scope.clone(),
                    CompareExecutionState::Expired {
                        verification_epoch: 0,
                        launched_run_id: Some(identity.compare_run_id),
                        reason: CompareExecutionExpiryReasonDto::ApplicationRestarted,
                    },
                )
            })
            .collect();
        Self {
            versions_by_id: HashMap::new(),
            cache_order: std::collections::VecDeque::new(),
            retained_identities_by_id: loaded.identities_by_id,
            cache_capacity: HOT_RESULT_CACHE_CAPACITY,
            latest_by_scope: loaded.latest_by_scope,
            execution_by_scope,
            job_names: loaded.job_names,
            persistence_generation: loaded.generation,
        }
    }
}
