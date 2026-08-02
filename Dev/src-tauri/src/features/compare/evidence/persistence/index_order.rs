//! Stable persisted ordering for latest-scope pointers.

use std::cmp::Ordering;

use super::schema::LatestResult;

pub(super) fn compare_latest_scope(left: &LatestResult, right: &LatestResult) -> Ordering {
    (&left.job_id, left.target_index, &left.config_revision).cmp(&(
        &right.job_id,
        right.target_index,
        &right.config_revision,
    ))
}
