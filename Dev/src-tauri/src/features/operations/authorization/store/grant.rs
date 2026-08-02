//! Session-grant lookup with least-recently-used touch semantics.

use super::super::apply::ApplyReview;
use super::super::compare::CompareAuthorization;
use super::retention::touch_grant;
use super::state::GrantRecord;
use super::OperationAuthorizationStore;

impl OperationAuthorizationStore {
    pub(crate) fn has_compare_capability_grant(
        &self,
        authorization: &CompareAuthorization,
    ) -> bool {
        self.find_grant(|grant| grant.allows_compare(authorization))
    }

    pub(crate) fn has_interactive_apply_capability_grant(&self, review: &ApplyReview) -> bool {
        self.find_grant(|grant| grant.allows_apply(review, false))
    }

    pub(super) fn find_grant(&self, predicate: impl Fn(&GrantRecord) -> bool) -> bool {
        let mut state = self.0.lock().unwrap();
        let Some(index) = state.grants.iter().position(predicate) else {
            return false;
        };
        touch_grant(&mut state.grants, index);
        true
    }
}
