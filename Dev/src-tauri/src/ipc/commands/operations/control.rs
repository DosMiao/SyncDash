//! Cancellation, pause, and replay controls for active runs.

use std::sync::Arc;

use crate::contracts::events::{RunEventDto, RunEventPurposeDto};
use crate::features::operations::events::repository::RunEventRepository;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;
use crate::features::operations::lifecycle::model::RunPurpose;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub fn cancel_compare_run(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    run_id: u64,
) -> Result<bool, String> {
    require_window_role(&window, WindowRole::Main)?;
    lifecycle.request_cancel(run_id, RunPurpose::Compare)
}

#[tauri::command]
pub fn cancel_apply_run(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    run_id: u64,
) -> Result<bool, String> {
    require_window_role(&window, WindowRole::Progress)?;
    lifecycle.request_cancel(run_id, RunPurpose::Apply)
}

#[tauri::command]
pub fn set_apply_paused(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    run_id: u64,
    paused: bool,
) -> Result<bool, String> {
    require_window_role(&window, WindowRole::Progress)?;
    lifecycle.set_paused(run_id, RunPurpose::Apply, paused)
}

#[tauri::command]
pub fn replay_compare_events(
    window: tauri::WebviewWindow,
    events: tauri::State<'_, Arc<RunEventRepository>>,
    after_sequence: Option<u64>,
) -> Result<Vec<RunEventDto>, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(events.replay(RunEventPurposeDto::Compare, after_sequence.unwrap_or(0)))
}

#[tauri::command]
pub fn replay_apply_events(
    window: tauri::WebviewWindow,
    events: tauri::State<'_, Arc<RunEventRepository>>,
    after_sequence: Option<u64>,
) -> Result<Vec<RunEventDto>, String> {
    require_window_role(&window, WindowRole::Progress)?;
    Ok(events.replay(RunEventPurposeDto::Apply, after_sequence.unwrap_or(0)))
}
