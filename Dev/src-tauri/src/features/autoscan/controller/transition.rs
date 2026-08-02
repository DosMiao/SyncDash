//! Durable verification transitions and their UI delivery.

use tauri::Emitter;

use crate::contracts::compare::CompareScopeExecutionStatusDto;
use crate::features::autoscan::model::{AutoScanBinding, AutoScanVerificationTerminal};
use crate::features::autoscan::runtime::AutoScanExecutionServices;
use crate::features::compare::evidence::model::verification::CompareVerificationTicket;
use crate::window::MAIN_WINDOW_LABEL;

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
