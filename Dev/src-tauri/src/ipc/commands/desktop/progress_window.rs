use std::sync::Arc;

use crate::features::operations::lifecycle::coordinator::RunLifecycle;
use crate::features::operations::lifecycle::model::ProgressWindowCloseDecisionDto;
use crate::ipc::{require_window_role, WindowRole};
use crate::window::PROGRESS_WINDOW_LABEL;

#[tauri::command]
pub async fn open_progress_window(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
) -> Result<u64, String> {
    require_window_role(&window, WindowRole::Main)?;
    let lifecycle = lifecycle.inner().clone();
    let launch_id = lifecycle.reserve_progress_launch()?;
    match prepare_progress_window(&app, &lifecycle, launch_id).await {
        Ok(()) => Ok(launch_id),
        Err(error) => {
            lifecycle.cancel_progress_launch(launch_id);
            Err(error)
        }
    }
}

async fn wait_for_window_signal(
    receiver: std::sync::mpsc::Receiver<()>,
    failure: &'static str,
) -> Result<(), String> {
    let received = tauri::async_runtime::spawn_blocking(move || {
        receiver.recv_timeout(std::time::Duration::from_secs(5))
    })
    .await
    .map_err(|error| error.to_string())?;
    received.map_err(|_| failure.to_string())
}

async fn prepare_progress_window(
    app: &tauri::AppHandle,
    lifecycle: &RunLifecycle,
    launch_id: u64,
) -> Result<(), String> {
    use tauri::{Emitter, Manager};

    let window = if let Some(window) = app.get_webview_window(PROGRESS_WINDOW_LABEL) {
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        window
    } else {
        let mounted_rx = lifecycle.prepare_progress_window_mount(launch_id)?;
        let built = tauri::WebviewWindowBuilder::new(
            app,
            PROGRESS_WINDOW_LABEL,
            tauri::WebviewUrl::App(format!("progress.html?launch_id={launch_id}").into()),
        )
        .title("SyncDash — Run")
        .inner_size(620.0, 500.0)
        .min_inner_size(440.0, 380.0)
        .build()
        .map_err(|error| error.to_string());
        let window = built?;
        if let Err(error) = wait_for_window_signal(
            mounted_rx,
            "Progress window did not load; synchronization was not started",
        )
        .await
        {
            let _ = window.destroy();
            return Err(error);
        }
        window
    };

    let ready_rx = lifecycle.prepare_progress_launch_acknowledgement(launch_id)?;
    if let Err(error) = window.emit("progress-window-arm", launch_id) {
        let _ = window.destroy();
        return Err(error.to_string());
    }
    if let Err(error) = wait_for_window_signal(
        ready_rx,
        "Progress window did not acknowledge this run; synchronization was not started",
    )
    .await
    {
        let _ = window.destroy();
        return Err(error);
    }
    Ok(())
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
    use tauri::Manager;
    let result = match app.get_webview_window(PROGRESS_WINDOW_LABEL) {
        Some(window) => window.destroy().map_err(|error| error.to_string()),
        None => Ok(()),
    };
    if result.is_ok() {
        lifecycle.finish_progress_window_close();
    }
    result
}
