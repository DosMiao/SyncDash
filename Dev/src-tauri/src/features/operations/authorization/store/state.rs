//! Records, capacities, and session-grant matching for the authority store.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::super::apply::OperationAuthorization;
use super::super::challenge::{IssuedAuthorization, ReviewChallenge};

pub(super) const CHALLENGE_TTL: Duration = Duration::from_secs(10 * 60);
pub(super) const AUTHORIZATION_TTL: Duration = Duration::from_secs(2 * 60);
pub(super) const CHALLENGE_CAPACITY: usize = 32;
pub(super) const AUTHORIZATION_CAPACITY: usize = 32;

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

#[derive(Default)]
pub(super) struct AuthorizationState {
    pub(super) challenges: VecDeque<ChallengeRecord>,
    pub(super) authorizations: VecDeque<AuthorizationRecord>,
}
