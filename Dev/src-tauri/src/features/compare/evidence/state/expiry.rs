//! Single-scope execution expiry transition.

use crate::contracts::compare::CompareExecutionExpiryReasonDto;

use super::super::model::scope::CompareScope;
use super::CompareResultStore;

impl CompareResultStore {
    pub(in crate::features::compare::evidence) fn expire_scope(
        &mut self,
        scope: &CompareScope,
        reason: CompareExecutionExpiryReasonDto,
    ) {
        if let Some(state) = self.execution_by_scope.get_mut(scope) {
            state.expire(reason);
        }
    }
}
