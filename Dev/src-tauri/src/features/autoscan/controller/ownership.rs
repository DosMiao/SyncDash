//! Exact ownership predicates for AutoScan-generated Apply tickets.

use crate::features::autoscan::authority::AutoApplyTicket;
use crate::features::autoscan::runtime::ActiveAutoScan;

pub(super) fn active_owns_ticket(active: &ActiveAutoScan, ticket: &AutoApplyTicket) -> bool {
    let identity = ticket.compare_identity();
    active.generation == ticket.generation()
        && active.binding.auto_apply
        && active.binding.job_id == identity.job_id
        && active.binding.config_revision == identity.config_revision
        && active.binding.target_index == identity.target_index
}
