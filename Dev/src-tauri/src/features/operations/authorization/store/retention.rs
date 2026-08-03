//! Expiry and capacity trimming.

use std::collections::VecDeque;
use std::time::Instant;

use super::state::AuthorizationState;

pub(super) fn purge(state: &mut AuthorizationState, now: Instant) {
    state.challenges.retain(|record| record.expires > now);
    state.authorizations.retain(|record| record.expires > now);
}

pub(super) fn trim_front<T>(records: &mut VecDeque<T>, capacity: usize) {
    while records.len() > capacity {
        records.pop_front();
    }
}
