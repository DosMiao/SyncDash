//! Things that touch the desktop rather than the sync: revealing a path in the file manager,
//! the post-sync sleep/shutdown, and the progress sub-window.
//!
//! Window-creating commands are `async fn` deliberately — a sync command runs on the main thread
//! inside IPC, and wry needs the event loop pumping to finish creating a webview. A sync one
//! deadlocks the child at about:blank and queues close events behind the wedge.

use crate::dto::PostRunPowerActionDto;
use crate::run_lifecycle::RunLifecycle;
use crate::window_role::{require_window_role, WindowRole, PROGRESS_WINDOW_LABEL};

pub(crate) fn reveal_path(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("Path no longer exists: {}", path.display()));
    }
    #[cfg(windows)]
    {
        // explorer returns exit 1 even when the selection succeeds, so the status code is meaningless here — all that matters is whether the process started
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let directory = if path.is_dir() {
            path
        } else {
            path.parent().ok_or_else(|| {
                format!(
                    "Cannot determine the containing directory for {}",
                    path.display()
                )
            })?
        };
        std::process::Command::new("xdg-open")
            .arg(directory)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn execute_post_run_power_action(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, std::sync::Arc<RunLifecycle>>,
    run_id: u64,
    action: PostRunPowerActionDto,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Progress)?;
    lifecycle.consume_post_run_power_action_grant_with(run_id, || launch_power_action(action))
}

fn launch_power_action(action: PostRunPowerActionDto) -> Result<(), String> {
    #[cfg(windows)]
    let (prog, args): (&str, Vec<&str>) = {
        match action {
            PostRunPowerActionDto::Sleep => (
                "rundll32.exe",
                vec!["powrprof.dll,SetSuspendState", "0,1,0"],
            ),
            PostRunPowerActionDto::Shutdown => ("shutdown", vec!["/s", "/t", "5"]),
        }
    };
    #[cfg(target_os = "macos")]
    let (prog, args): (&str, Vec<&str>) = {
        match action {
            PostRunPowerActionDto::Sleep => ("pmset", vec!["sleepnow"]),
            PostRunPowerActionDto::Shutdown => (
                "osascript",
                vec!["-e", "tell application \"System Events\" to shut down"],
            ),
        }
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (prog, args): (&str, Vec<&str>) = {
        match action {
            PostRunPowerActionDto::Sleep => ("systemctl", vec!["suspend"]),
            PostRunPowerActionDto::Shutdown => ("systemctl", vec!["poweroff"]),
        }
    };
    std::process::Command::new(prog)
        .args(&args)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Open (or focus) the standalone progress sub-window (Synchronize only; compare progress is shown inline in the main window).
/// **Must be an async command**: sync commands run on the main thread's IPC, while wry needs the main event loop
/// to pump messages in order to create a window — creating it synchronously leaves the sub-window's navigation stuck
/// on about:blank (an all-white window) and close events never get queued (it looks like "the whole app won't close").
/// An async command runs on its own thread, so window creation is proxied through the event loop correctly.
#[tauri::command]
pub async fn open_progress_window(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    lifecycle: tauri::State<'_, std::sync::Arc<RunLifecycle>>,
) -> Result<u64, String> {
    require_window_role(&window, WindowRole::Main)?;
    let lifecycle = lifecycle.inner().clone();
    let launch_id = lifecycle.reserve_progress_launch()?;
    match prepare_progress_window(&app, &lifecycle, launch_id).await {
        Ok(()) => Ok(launch_id),
        Err(e) => {
            lifecycle.cancel_progress_launch(launch_id);
            Err(e)
        }
    }
}

async fn wait_for_window_signal(
    rx: std::sync::mpsc::Receiver<()>,
    failure: &'static str,
) -> Result<(), String> {
    let received = tauri::async_runtime::spawn_blocking(move || {
        rx.recv_timeout(std::time::Duration::from_secs(5))
    })
    .await
    .map_err(|e| e.to_string())?;
    received.map_err(|_| failure.to_string())
}

async fn prepare_progress_window(
    app: &tauri::AppHandle,
    lifecycle: &RunLifecycle,
    launch_id: u64,
) -> Result<(), String> {
    use tauri::{Emitter, Manager};

    let window = if let Some(window) = app.get_webview_window(PROGRESS_WINDOW_LABEL) {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
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
        .map_err(|e| e.to_string());
        let window = match built {
            Ok(window) => window,
            Err(e) => return Err(e),
        };
        let mounted = wait_for_window_signal(
            mounted_rx,
            "Progress window did not load; synchronization was not started",
        )
        .await;
        if let Err(e) = mounted {
            let _ = window.destroy();
            return Err(e);
        }
        window
    };

    let ready_rx = lifecycle.prepare_progress_launch_acknowledgement(launch_id)?;
    if let Err(e) = window.emit("progress-window-arm", launch_id) {
        let _ = window.destroy();
        return Err(e.to_string());
    }
    let ready = wait_for_window_signal(
        ready_rx,
        "Progress window did not acknowledge this run; synchronization was not started",
    )
    .await;
    if let Err(e) = ready {
        let _ = window.destroy();
        return Err(e);
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_progress_launch(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, std::sync::Arc<RunLifecycle>>,
    launch_id: u64,
) -> Result<bool, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(lifecycle.cancel_progress_launch(launch_id))
}

#[tauri::command]
pub fn report_progress_window_mounted(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, std::sync::Arc<RunLifecycle>>,
    launch_id: u64,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Progress)?;
    lifecycle.report_progress_window_mounted(launch_id)
}

#[tauri::command]
pub fn acknowledge_progress_launch(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, std::sync::Arc<RunLifecycle>>,
    launch_id: u64,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Progress)?;
    lifecycle.acknowledge_progress_launch(launch_id)
}

#[tauri::command]
pub fn begin_progress_window_close(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, std::sync::Arc<RunLifecycle>>,
) -> Result<crate::run_lifecycle::ProgressWindowCloseDecisionDto, String> {
    require_window_role(&window, WindowRole::Progress)?;
    Ok(lifecycle.begin_progress_window_close())
}

/// Destroy the progress sub-window. Not hide: a hidden sub-window keeps the process alive after the main window closes
#[tauri::command]
pub async fn destroy_progress_window(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    lifecycle: tauri::State<'_, std::sync::Arc<RunLifecycle>>,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Progress)?;
    use tauri::Manager;
    let result = match app.get_webview_window(PROGRESS_WINDOW_LABEL) {
        Some(w) => w.destroy().map_err(|e| e.to_string()),
        None => Ok(()),
    };
    if result.is_ok() {
        lifecycle.finish_progress_window_close();
    }
    result
}
