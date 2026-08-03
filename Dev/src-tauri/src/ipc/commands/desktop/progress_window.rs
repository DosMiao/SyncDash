use std::sync::Arc;

use crate::features::operations::lifecycle::model::ProgressWindowCloseDecisionDto;
use crate::features::operations::lifecycle::progress_window;
use crate::features::operations::lifecycle::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub async fn open_progress_window(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
) -> Result<u64, String> {
    require_window_role(&window, WindowRole::Main)?;
    let lifecycle = lifecycle.inner().clone();
    let launch_id = lifecycle.reserve_progress_launch()?;
    match progress_window::prepare(&app, &lifecycle, launch_id).await {
        Ok(()) => Ok(launch_id),
        Err(error) => {
            lifecycle.cancel_progress_launch(launch_id);
            Err(error)
        }
    }
}

#[tauri::command]
pub fn cancel_progress_launch(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    launch_id: u64,
) -> Result<bool, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(lifecycle.cancel_progress_launch(launch_id))
}

#[tauri::command]
pub fn report_progress_window_mounted(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    launch_id: u64,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Progress)?;
    lifecycle.report_progress_window_mounted(launch_id)
}

#[tauri::command]
pub fn acknowledge_progress_launch(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    launch_id: u64,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Progress)?;
    lifecycle.acknowledge_progress_launch(launch_id)
}

#[tauri::command]
pub fn begin_progress_window_close(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
) -> Result<ProgressWindowCloseDecisionDto, String> {
    require_window_role(&window, WindowRole::Progress)?;
    Ok(lifecycle.begin_progress_window_close())
}

#[tauri::command]
pub async fn destroy_progress_window(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Progress)?;
    progress_window::destroy(&app, lifecycle.inner())
}
