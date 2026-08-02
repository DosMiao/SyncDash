//! Challenge issuance, acknowledgement validation, and optional grant commit.

use std::time::Instant;

use super::super::apply::{InteractiveApplyAuthorization, OperationAuthorization};
use super::super::challenge::{
    ApplySessionGrantDecision, IssuedAuthorization, IssuedChallenge, ReviewApproval,
    ReviewChallenge,
};
use super::issuance::{commit_authorization, prepare_authorization, random_token, wall_expiry_ms};
use super::retention::{purge, trim_front};
use super::state::{
    ChallengeRecord, GrantRecord, GrantScope, CHALLENGE_CAPACITY, CHALLENGE_TTL, GRANT_CAPACITY,
};
use super::OperationAuthorizationStore;

impl OperationAuthorizationStore {
    pub(crate) fn create_review_challenge(
        &self,
        challenge: ReviewChallenge,
    ) -> Result<IssuedChallenge, String> {
        self.create_review_challenge_at(challenge, Instant::now())
    }

    fn create_review_challenge_at(
        &self,
        challenge: ReviewChallenge,
        now: Instant,
    ) -> Result<IssuedChallenge, String> {
        let id = random_token()?;
        let mut state = self.0.lock().unwrap();
        purge(&mut state, now);
        state.challenges.push_back(ChallengeRecord {
            id: id.clone(),
            challenge,
            expires: now + CHALLENGE_TTL,
        });
        trim_front(&mut state.challenges, CHALLENGE_CAPACITY);
        Ok(IssuedChallenge {
            challenge_id: id,
            expires_at_ms: wall_expiry_ms(CHALLENGE_TTL),
        })
    }

    pub(crate) fn approve_review_challenge(
        &self,
        challenge_id: &str,
        approval: ReviewApproval,
    ) -> Result<IssuedAuthorization, String> {
        self.approve_review_challenge_at(challenge_id, approval, Instant::now())
    }

    fn approve_review_challenge_at(
        &self,
        challenge_id: &str,
        approval: ReviewApproval,
        now: Instant,
    ) -> Result<IssuedAuthorization, String> {
        self.approve_review_challenge_at_with_token(challenge_id, approval, now, random_token)
    }

    pub(super) fn approve_review_challenge_at_with_token(
        &self,
        challenge_id: &str,
        approval: ReviewApproval,
        now: Instant,
        create_token: impl FnOnce() -> Result<String, String>,
    ) -> Result<IssuedAuthorization, String> {
        let mut state = self.0.lock().unwrap();
        let index = state
            .challenges
            .iter()
            .position(|challenge| challenge.id == challenge_id)
            .ok_or_else(|| "This review challenge expired or was already used".to_string())?;
        // Burn before expiry, approval-shape, or acknowledgement inspection.
        let challenge = state
            .challenges
            .remove(index)
            .expect("a located challenge must exist");
        if challenge.expires <= now {
            return Err("This review challenge expired — review again".into());
        }

        let (authorization, grant) = match (challenge.challenge, approval) {
            (
                ReviewChallenge::Compare {
                    authorization,
                    requires_capability_ack,
                },
                ReviewApproval::Compare {
                    accept_capabilities,
                    remember_for_session,
                },
            ) => {
                if requires_capability_ack && !accept_capabilities {
                    return Err("The reviewed capability limitations were not accepted".into());
                }
                if remember_for_session && !requires_capability_ack {
                    return Err(
                        "There is no reviewed Compare capability limitation to remember".into(),
                    );
                }
                let grant = remember_for_session.then(|| GrantRecord {
                    scope: GrantScope::Compare,
                    target: authorization.target().clone(),
                    capability_review_digest: authorization.capability_review_digest().to_string(),
                    allow_auto_apply: false,
                });
                (OperationAuthorization::Compare(authorization), grant)
            }
            (
                ReviewChallenge::InteractiveApply {
                    review,
                    requires_health_ack,
                    requires_capability_ack,
                },
                ReviewApproval::InteractiveApply {
                    acknowledge_health,
                    accept_capabilities,
                    session_grant,
                },
            ) => {
                if requires_health_ack && !acknowledge_health {
                    return Err("The reviewed health warning was not acknowledged".into());
                }
                if requires_capability_ack && !accept_capabilities {
                    return Err("The reviewed capability limitations were not accepted".into());
                }
                let allow_auto_apply = session_grant == ApplySessionGrantDecision::AllowAutoApply;
                let grant =
                    (session_grant != ApplySessionGrantDecision::None).then(|| GrantRecord {
                        scope: GrantScope::Apply,
                        target: review.target(),
                        capability_review_digest: review.capability_review_digest().to_string(),
                        allow_auto_apply,
                    });
                (
                    OperationAuthorization::InteractiveApply(InteractiveApplyAuthorization::new(
                        review,
                        acknowledge_health,
                    )),
                    grant,
                )
            }
            _ => return Err("This approval belongs to a different review operation".into()),
        };

        let prepared = prepare_authorization(authorization, now, create_token)?;

        if let Some(grant) = grant {
            if let Some(existing) = state
                .grants
                .iter()
                .position(|candidate| candidate.same_key(&grant))
            {
                state.grants.remove(existing);
            }
            if grant.scope == GrantScope::Apply && !grant.allow_auto_apply {
                state
                    .authorizations
                    .retain(|record| match &record.authorization {
                        OperationAuthorization::AutoApply(authorization) => {
                            !grant.allows_apply(authorization.review(), false)
                        }
                        _ => true,
                    });
            }
            state.grants.push_back(grant);
            trim_front(&mut state.grants, GRANT_CAPACITY);
        }

        Ok(commit_authorization(&mut state, prepared))
    }
}
