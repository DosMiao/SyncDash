//! Bringing the progress window up and proving it is listening before a run starts.
//!
//! Five phases, and the ordering is the safety property: reserve a launch id, prepare the mount
//! channel *before* building the window, build it, wait for the webview to report mounted, then
//! arm it and wait for the acknowledgement. Only after the acknowledgement may the caller start
//! writing. A run whose progress window never armed would execute with no visible progress and no
//! reachable cancel control.
//!
//! Both waits are bounded at five seconds, and every failure path from the arm signal onward
//! destroys the window. A half-built progress window left on screen is worse than none: it looks
//! like a run that is being watched.
//!
//! This is desktop lifecycle policy, not delivery: it is the only place that builds the progress
//! window, so nothing can put one on screen without completing this handshake.

use tauri::{Emitter, Manager};

use crate::features::operations::lifecycle::RunLifecycle;
use crate::window::PROGRESS_WINDOW_LABEL;

/// How long the mount and acknowledgement signals may take before the launch is abandoned.
const WINDOW_SIGNAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn wait_for_window_signal(
    receiver: std::sync::mpsc::Receiver<()>,
    failure: &'static str,
) -> Result<(), String> {
    let received =
        tauri::async_runtime::spawn_blocking(move || receiver.recv_timeout(WINDOW_SIGNAL_TIMEOUT))
            .await
            .map_err(|error| error.to_string())?;
    received.map_err(|_| failure.to_string())
}

/// Show or build the progress window, then arm it for `launch_id` and wait for it to acknowledge.
pub(crate) async fn prepare(
    app: &tauri::AppHandle,
    lifecycle: &RunLifecycle,
    launch_id: u64,
) -> Result<(), String> {
    let window = if let Some(window) = app.get_webview_window(PROGRESS_WINDOW_LABEL) {
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        window
    } else {
        // The mount channel is prepared before the build, so a webview that loads fast cannot
        // report mounted into a channel that does not exist yet.
        let mounted_rx = lifecycle.prepare_progress_window_mount(launch_id)?;
        let window = tauri::WebviewWindowBuilder::new(
            app,
            PROGRESS_WINDOW_LABEL,
            tauri::WebviewUrl::App(format!("progress.html?launch_id={launch_id}").into()),
        )
        .title("SyncDash — Run")
        .inner_size(620.0, 500.0)
        .min_inner_size(440.0, 380.0)
        .build()
        .map_err(|error| error.to_string())?;
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

/// Destroy the progress window and close out its lifecycle, if it is still open.
pub(crate) fn destroy(app: &tauri::AppHandle, lifecycle: &RunLifecycle) -> Result<(), String> {
    let result = match app.get_webview_window(PROGRESS_WINDOW_LABEL) {
        Some(window) => window.destroy().map_err(|error| error.to_string()),
        None => Ok(()),
    };
    if result.is_ok() {
        lifecycle.finish_progress_window_close();
    }
    result
}
