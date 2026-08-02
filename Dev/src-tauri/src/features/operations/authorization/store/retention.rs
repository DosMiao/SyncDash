//! Expiry, capacity trimming, and least-recently-used grant order.

use std::collections::VecDeque;
use std::time::Instant;

use super::state::{AuthorizationState, GrantRecord};

pub(super) fn purge(state: &mut AuthorizationState, now: Instant) {
    state.challenges.retain(|record| record.expires > now);
    state.authorizations.retain(|record| record.expires > now);
}

pub(super) fn trim_front<T>(records: &mut VecDeque<T>, capacity: usize) {
    while records.len() > capacity {
        records.pop_front();
    }
}

pub(super) fn touch_grant(grants: &mut VecDeque<GrantRecord>, index: usize) {
    if index + 1 != grants.len() {
        let grant = grants.remove(index).expect("a located grant must exist");
        grants.push_back(grant);
    }
}
