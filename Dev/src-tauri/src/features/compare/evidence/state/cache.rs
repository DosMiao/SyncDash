//! Exact-identity hot cache and retained presentation projection.

use std::sync::Arc;

use crate::contracts::compare::{CompareIdentity, CompareOwner};

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::result::{CompareResultVersion, RetainedCompareResult};
use super::CompareResultStore;

impl CompareResultStore {
    pub(in crate::features::compare::evidence) fn cached_exact(
        &mut self,
        identity: &CompareIdentity,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        if !self.exact_identity_is_retained(identity)? {
            return Ok(None);
        }
        let Some(version) = self.versions_by_id.get(&identity.result_id).cloned() else {
            return Ok(None);
        };
        self.touch_cache(&identity.result_id);
        self.retained_result(version).map(Some)
    }

    pub(in crate::features::compare::evidence) fn exact_identity_is_retained(
        &self,
        identity: &CompareIdentity,
    ) -> Result<bool, CompareResultRepositoryError> {
        match self.retained_identities_by_id.get(&identity.result_id) {
            None => Ok(false),
            Some(retained) if retained == identity => Ok(true),
            Some(_) => Err(CompareResultRepositoryError::IdentityMismatch {
                result_id: identity.result_id.clone(),
            }),
        }
    }

    pub(in crate::features::compare::evidence) fn cache_version(
        &mut self,
        version: Arc<CompareResultVersion>,
    ) {
        let result_id = version.identity.result_id.clone();
        self.versions_by_id.insert(result_id.clone(), version);
        self.touch_cache(&result_id);
        while self.cache_order.len() > self.cache_capacity {
            let evicted = self
                .cache_order
                .pop_back()
                .expect("an over-capacity result cache contains an entry");
            self.versions_by_id.remove(&evicted);
        }
    }

    fn touch_cache(&mut self, result_id: &str) {
        if let Some(index) = self
            .cache_order
            .iter()
            .position(|cached| cached == result_id)
        {
            self.cache_order.remove(index);
        }
        self.cache_order.push_front(result_id.to_string());
    }

    pub(in crate::features::compare::evidence) fn retained_result(
        &self,
        version: Arc<CompareResultVersion>,
    ) -> Result<RetainedCompareResult, CompareResultRepositoryError> {
        let job_name = self
            .job_names
            .get(&version.identity.job_id)
            .cloned()
            .ok_or_else(|| CompareResultRepositoryError::MissingJobDisplayName {
                job_id: version.identity.job_id.clone(),
            })?;
        Ok(RetainedCompareResult {
            owner: CompareOwner {
                identity: version.identity.clone(),
                job_name,
            },
            version,
        })
    }
}
