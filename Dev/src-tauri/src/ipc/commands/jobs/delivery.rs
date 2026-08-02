use tauri::Emitter;

use crate::features::jobs::mutation::model::JobMutationStatusEvents;
use crate::window::MAIN_WINDOW_LABEL;

pub(super) fn deliver_statuses(
    app: &tauri::AppHandle,
    events: &JobMutationStatusEvents,
) -> Vec<String> {
    let mut failures = Vec::new();
    if let Some(status) = &events.autoscan {
        if let Err(error) = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", status) {
            failures.push(format!("autoscan-status: {error}"));
        }
    }
    for status in &events.compare_execution {
        if let Err(error) = app.emit_to(MAIN_WINDOW_LABEL, "compare-execution-status", status) {
            failures.push(format!("compare-execution-status: {error}"));
        }
    }
    failures
}
