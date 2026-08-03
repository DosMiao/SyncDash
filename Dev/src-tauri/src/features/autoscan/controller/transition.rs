//! Durable verification transitions and their UI delivery.

use tauri::Emitter;

use crate::contracts::compare::CompareScopeExecutionStatusDto;
use crate::features::autoscan::model::{
    AutoScanBinding, AutoScanStatusDto, AutoScanVerificationTerminal,
};
use crate::features::autoscan::runtime::{ActiveAutoScan, AutoScanExecutionServices};
use crate::features::autoscan::worker::observation::mark_shared_inactive;
use crate::features::compare::evidence::model::verification::CompareVerificationTicket;
use crate::window::MAIN_WINDOW_LABEL;

/// Cancel whatever verification a generation still owns and mark its shared status inactive.
/// Stopping a generation and dropping the controller must terminalize it the same way, so both
/// take that step from here and add only their own extra cleanup around it.
pub(super) fn deactivate_generation(
    execution: &AutoScanExecutionServices,
    active: &ActiveAutoScan,
    detail: &str,
) -> (AutoScanStatusDto, Option<CompareScopeExecutionStatusDto>) {
    let mut shared = active.shared.lock().unwrap();
    let execution_status = shared.ticket.verification().and_then(|verification| {
        terminalize_repository_verification(
            execution,
            &active.binding,
            verification,
            &AutoScanVerificationTerminal::Cancelled,
        )
    });
    (mark_shared_inactive(&mut shared, detail), execution_status)
}

pub(in crate::features::autoscan) fn terminalize_repository_verification(
    execution: &AutoScanExecutionServices,
    binding: &AutoScanBinding,
    verification: &CompareVerificationTicket,
    terminal: &AutoScanVerificationTerminal,
) -> Option<CompareScopeExecutionStatusDto> {
    execution
        .results
        .complete_verification_terminal(verification, terminal.repository_outcome())
        .then(|| execution.results.execution_status(&binding.compare_scope()))
}

pub(in crate::features::autoscan) fn emit_execution_transition(
    app: &tauri::AppHandle,
    status: Option<CompareScopeExecutionStatusDto>,
) {
    if let Some(status) = status {
        let _ = app.emit_to(MAIN_WINDOW_LABEL, "compare-execution-status", status);
    }
}
