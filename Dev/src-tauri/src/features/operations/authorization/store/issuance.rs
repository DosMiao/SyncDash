//! Atomic authorization-token preparation and commit.

use std::time::{Duration, Instant};

use super::super::apply::OperationAuthorization;
use super::super::challenge::IssuedAuthorization;
use super::retention::trim_front;
use super::state::{
    AuthorizationRecord, AuthorizationState, PreparedAuthorization, AUTHORIZATION_CAPACITY,
    AUTHORIZATION_TTL,
};

pub(super) fn issue_into(
    state: &mut AuthorizationState,
    authorization: OperationAuthorization,
    now: Instant,
) -> Result<IssuedAuthorization, String> {
    let prepared = prepare_authorization(authorization, now, random_token)?;
    Ok(commit_authorization(state, prepared))
}

pub(super) fn prepare_authorization(
    authorization: OperationAuthorization,
    now: Instant,
    create_token: impl FnOnce() -> Result<String, String>,
) -> Result<PreparedAuthorization, String> {
    let token = create_token()?;
    Ok(PreparedAuthorization {
        record: AuthorizationRecord {
            token: token.clone(),
            authorization,
            expires: now + AUTHORIZATION_TTL,
        },
        issued: IssuedAuthorization {
            authorization_token: token,
            expires_at_ms: wall_expiry_ms(AUTHORIZATION_TTL),
        },
    })
}

pub(super) fn commit_authorization(
    state: &mut AuthorizationState,
    prepared: PreparedAuthorization,
) -> IssuedAuthorization {
    state.authorizations.push_back(prepared.record);
    trim_front(&mut state.authorizations, AUTHORIZATION_CAPACITY);
    prepared.issued
}

pub(super) fn random_token() -> Result<String, String> {
    crate::secure_random::random_hex::<32>("Cannot create an operation authorization")
}

pub(super) fn wall_expiry_ms(ttl: Duration) -> u64 {
    syncdash::foundation::time::now_ms().saturating_add(ttl.as_millis() as u64)
}
