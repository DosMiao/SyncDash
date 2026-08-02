//! Worker cadence and monotonic ticket configuration.

use std::time::Duration;

pub(in crate::features::autoscan) const DEFAULT_INTERVAL_SECS: u64 = 30;
pub(super) const WORKER_TICK: Duration = Duration::from_millis(100);

pub(in crate::features::autoscan) fn next_ticket_id(ticket_id: u64) -> Option<u64> {
    ticket_id.checked_add(1)
}

pub(crate) fn configured_interval(job: &syncdash::job::Job) -> u64 {
    job.autoscan_interval_secs
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(1)
}
