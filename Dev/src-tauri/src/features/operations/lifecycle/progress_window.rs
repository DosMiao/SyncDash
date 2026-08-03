//! Bringing the progress window up and proving it is listening before a run starts.
//!
//! Five phases, and the ordering is the safety property: reserve a launch id, prepare the mount
//! channel *before* building the window, build it, wait for the webview to report mounted, then
//! arm it and wait for the acknowledgement. Only after the acknowledgement may the caller start
//! writing. A run whose progress window never armed would execute with no visible progress and no
//! reachable cancel control.
//!
//! Both waits are bounded at five seconds, and once the window exists every failure path tears it
//! down. A half-built progress window left on screen is worse than none: it looks like a run that
//! is being watched.
//!
//! This is desktop lifecycle policy, not delivery: it is the only place that builds the progress
//! window and the only place that removes one. Nothing can put one on screen without completing
//! this handshake, and nothing can take one away without settling the close it belonged to.

use std::sync::mpsc::Receiver;

use tauri::{Emitter, Manager};

use crate::features::operations::lifecycle::RunLifecycle;
use crate::window::PROGRESS_WINDOW_LABEL;

/// How long the mount and acknowledgement signals may take before the launch is abandoned.
const WINDOW_SIGNAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

const MOUNT_FAILURE: &str = "Progress window did not load; synchronization was not started";
const ACKNOWLEDGEMENT_FAILURE: &str =
    "Progress window did not acknowledge this run; synchronization was not started";

/// What arming and teardown do to the progress window. `tauri::WebviewWindow` is the only
/// production implementation; the trait exists because whether a failed handshake leaves a window
/// on screen — and whether it leaves the lifecycle able to start another run — has to be provable
/// without a webview.
pub(super) trait ProgressWindowSurface {
    fn emit_arm(&self, launch_id: u64) -> Result<(), String>;
    fn destroy_window(&self) -> Result<(), String>;
}

impl<R: tauri::Runtime> ProgressWindowSurface for tauri::WebviewWindow<R> {
    fn emit_arm(&self, launch_id: u64) -> Result<(), String> {
        self.emit("progress-window-arm", launch_id)
            .map_err(|error| error.to_string())
    }

    fn destroy_window(&self) -> Result<(), String> {
        self.destroy().map_err(|error| error.to_string())
    }
}

async fn wait_for_window_signal(
    receiver: Receiver<()>,
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
    // Acquisition only. Nothing that fails here has put a window on screen: the reuse branch found
    // one that was already there, and the build branch has not built one yet.
    let (window, mounted_rx) = match app.get_webview_window(PROGRESS_WINDOW_LABEL) {
        Some(window) => {
            window.show().map_err(|error| error.to_string())?;
            window.set_focus().map_err(|error| error.to_string())?;
            (window, None)
        }
        None => {
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
            (window, Some(mounted_rx))
        }
    };
    arm(&window, lifecycle, launch_id, mounted_rx).await
}

/// A window exists from here on, so one place decides its fate rather than each `?` deciding for
/// itself: anything the handshake reports, including a step a later edit adds to it, tears the
/// window down.
pub(super) async fn arm(
    window: &impl ProgressWindowSurface,
    lifecycle: &RunLifecycle,
    launch_id: u64,
    mounted_rx: Option<Receiver<()>>,
) -> Result<(), String> {
    match handshake(window, lifecycle, launch_id, mounted_rx).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = tear_down(Some(window), lifecycle);
            Err(error)
        }
    }
}

/// Wait for the mount signal when this call built the window, then arm it for `launch_id` and wait
/// for the acknowledgement.
async fn handshake(
    window: &impl ProgressWindowSurface,
    lifecycle: &RunLifecycle,
    launch_id: u64,
    mounted_rx: Option<Receiver<()>>,
) -> Result<(), String> {
    if let Some(mounted_rx) = mounted_rx {
        wait_for_window_signal(mounted_rx, MOUNT_FAILURE).await?;
    }
    let ready_rx = lifecycle.prepare_progress_launch_acknowledgement(launch_id)?;
    window.emit_arm(launch_id)?;
    wait_for_window_signal(ready_rx, ACKNOWLEDGEMENT_FAILURE).await
}

/// Destroy the progress window and close out its lifecycle, if it is still open.
pub(crate) fn destroy(app: &tauri::AppHandle, lifecycle: &RunLifecycle) -> Result<(), String> {
    tear_down(
        app.get_webview_window(PROGRESS_WINDOW_LABEL).as_ref(),
        lifecycle,
    )
}

/// The one place a progress window stops existing.
///
/// `progress_window_closing` is raised by the progress webview and lowered only by
/// `destroy_progress_window`, which is role-gated to that same webview. Removing the window
/// anywhere else therefore used to kill the only party that could lower it again, and
/// `reserve_progress_launch` refuses every Apply while it is raised — for the life of the process.
/// Removing the window and settling its close are one operation, and every route that removes the
/// window comes through here.
fn tear_down(
    window: Option<&impl ProgressWindowSurface>,
    lifecycle: &RunLifecycle,
) -> Result<(), String> {
    let removed = window.map_or(Ok(()), ProgressWindowSurface::destroy_window);
    if removed.is_ok() {
        lifecycle.finish_progress_window_close();
    }
    removed
}
