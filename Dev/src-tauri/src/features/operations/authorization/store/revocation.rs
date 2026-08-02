//! Exact authority revocation after registry or evidence freshness changes.

use crate::features::compare::evidence::model::scope::CompareScope;

use super::super::apply::OperationAuthorization;
use super::super::challenge::ReviewChallenge;
use super::OperationAuthorizationStore;

impl OperationAuthorizationStore {
    pub(crate) fn revoke_job_authority(&self, job_id: &str) {
        let mut state = self.0.lock().unwrap();
        state.challenges.retain(|record| match &record.challenge {
            ReviewChallenge::Compare { authorization, .. } => {
                authorization.target().job_id() != job_id
            }
            ReviewChallenge::InteractiveApply { review, .. } => {
                review.compare_identity().job_id != job_id
            }
        });
        state
            .authorizations
            .retain(|record| match &record.authorization {
                OperationAuthorization::Compare(authorization) => {
                    authorization.target().job_id() != job_id
                }
                OperationAuthorization::InteractiveApply(authorization) => {
                    authorization.review().compare_identity().job_id != job_id
                }
                OperationAuthorization::AutoApply(authorization) => {
                    authorization.review().compare_identity().job_id != job_id
                }
            });
        state
            .grants
            .retain(|record| record.target.job_id() != job_id);
    }

    pub(crate) fn revoke_apply_authority(&self, scope: &CompareScope) {
        let mut state = self.0.lock().unwrap();
        state.challenges.retain(|record| match &record.challenge {
            ReviewChallenge::Compare { .. } => true,
            ReviewChallenge::InteractiveApply { review, .. } => {
                !scope.contains(review.compare_identity())
            }
        });
        state
            .authorizations
            .retain(|record| match &record.authorization {
                OperationAuthorization::Compare(_) => true,
                OperationAuthorization::InteractiveApply(authorization) => {
                    !scope.contains(authorization.review().compare_identity())
                }
                OperationAuthorization::AutoApply(authorization) => {
                    !scope.contains(authorization.review().compare_identity())
                }
            });
        // Session grants record reviewed capability consent, not evidence freshness. Keeping them
        // lets a successful new Compare restore unattended operation without weakening the result gate.
    }
}
