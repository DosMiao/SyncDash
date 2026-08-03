//! Exact-identity result lookup.

use crate::contracts::compare::CompareIdentity;

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::result::RetainedCompareResult;
use super::CompareResultRepository;

impl CompareResultRepository {
    pub(crate) fn get_exact(
        &self,
        identity: &CompareIdentity,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        self.load_exact_locked(&mut store, identity)
    }

    /// The exact result, or the one refusal that explains its absence. Every caller that cannot
    /// proceed without the evidence goes through here, so a forgotten or evicted result is always
    /// reported the same way.
    pub(crate) fn require_exact(
        &self,
        identity: &CompareIdentity,
    ) -> Result<RetainedCompareResult, CompareResultRepositoryError> {
        self.get_exact(identity)?
            .ok_or(CompareResultRepositoryError::NotRetained)
    }

    pub(crate) fn get_fresh_exact(
        &self,
        identity: &CompareIdentity,
    ) -> Result<RetainedCompareResult, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        store.ensure_execution_fresh(identity)?;
        self.load_exact_locked(&mut store, identity)?
            .ok_or_else(|| {
                CompareResultRepositoryError::FreshResultWasNotRetained(identity.clone())
            })
    }
}
