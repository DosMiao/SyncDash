//! Challenge issuance, acknowledgement validation, and optional grant commit.

use std::time::Instant;

use super::super::apply::{InteractiveApplyAuthorization, OperationAuthorization};
use super::super::challenge::{
    IssuedAuthorization, IssuedChallenge, ReviewApproval, ReviewChallenge,
};
use super::issuance::{commit_authorization, prepare_authorization, random_token, wall_expiry_ms};
use super::retention::{purge, trim_front};
use super::state::{ChallengeRecord, CHALLENGE_CAPACITY, CHALLENGE_TTL};
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

        let authorization = match (challenge.challenge, approval) {
            (ReviewChallenge::InteractiveApply { review }, ReviewApproval::InteractiveApply) => {
                OperationAuthorization::InteractiveApply(InteractiveApplyAuthorization::new(review))
            }
        };

        let prepared = prepare_authorization(authorization, now, create_token)?;

        Ok(commit_authorization(&mut state, prepared))
    }
}
