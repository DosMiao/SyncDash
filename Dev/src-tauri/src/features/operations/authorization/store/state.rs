//! Records, capacities, and session-grant matching for the authority store.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::super::apply::{ApplyReview, OperationAuthorization};
use super::super::challenge::{IssuedAuthorization, ReviewChallenge};
use super::super::compare::CompareAuthorization;
use super::super::target::JobTargetRevision;

pub(super) const CHALLENGE_TTL: Duration = Duration::from_secs(10 * 60);
pub(super) const AUTHORIZATION_TTL: Duration = Duration::from_secs(2 * 60);
pub(super) const CHALLENGE_CAPACITY: usize = 32;
pub(super) const AUTHORIZATION_CAPACITY: usize = 32;
pub(super) const GRANT_CAPACITY: usize = 32;

#[derive(Clone, Debug)]
pub(super) struct ChallengeRecord {
    pub(super) id: String,
    pub(super) challenge: ReviewChallenge,
    pub(super) expires: Instant,
}

#[derive(Clone, Debug)]
pub(super) struct AuthorizationRecord {
    pub(super) token: String,
    pub(super) authorization: OperationAuthorization,
    pub(super) expires: Instant,
}

pub(super) struct PreparedAuthorization {
    pub(super) record: AuthorizationRecord,
    pub(super) issued: IssuedAuthorization,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GrantScope {
    Compare,
    Apply,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GrantRecord {
    pub(super) scope: GrantScope,
    pub(super) target: JobTargetRevision,
    pub(super) capability_review_digest: String,
    pub(super) allow_auto_apply: bool,
}

impl GrantRecord {
    pub(super) fn same_key(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.target == other.target
            && self.capability_review_digest == other.capability_review_digest
    }

    pub(super) fn allows_compare(&self, authorization: &CompareAuthorization) -> bool {
        self.scope == GrantScope::Compare
            && &self.target == authorization.target()
            && self.capability_review_digest == authorization.capability_review_digest()
    }

    pub(super) fn allows_apply(&self, review: &ApplyReview, auto_apply: bool) -> bool {
        self.scope == GrantScope::Apply
            && self.target == review.target()
            && self.capability_review_digest == review.capability_review_digest()
            && (!auto_apply || self.allow_auto_apply)
    }
}

#[derive(Default)]
pub(super) struct AuthorizationState {
    pub(super) challenges: VecDeque<ChallengeRecord>,
    pub(super) authorizations: VecDeque<AuthorizationRecord>,
    pub(super) grants: VecDeque<GrantRecord>,
}
