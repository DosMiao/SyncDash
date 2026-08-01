//! Things that touch the desktop rather than the sync: revealing a path in the file manager,
//! the post-sync sleep/shutdown, and the progress sub-window.
//!
//! Window-creating commands are `async fn` deliberately — a sync command runs on the main thread
//! inside IPC, and wry needs the event loop pumping to finish creating a webview. A sync one
//! deadlocks the child at about:blank and queues close events behind the wedge.

/// Select the path in the system file manager. Arguments go straight to the exe, not through a shell.
#[tauri::command]
pub fn reveal(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("Path no longer exists: {path}"));
    }
    #[cfg(windows)]
    {
        // explorer returns exit 1 even when the selection succeeds, so the status code is meaningless here — all that matters is whether the process started
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg("-R").arg(p).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if p.is_dir() { p } else { p.parent().unwrap_or(p) };
        std::process::Command::new("xdg-open").arg(dir).spawn().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// When-finished action (same as FFS): sleep / shutdown. The countdown and confirmation both happen in the frontend before this is called.
#[tauri::command]
pub fn post_sync_action(kind: String) -> Result<(), String> {
    #[cfg(windows)]
    let (prog, args): (&str, Vec<&str>) = {
        match kind.as_str() {
            "sleep" => ("rundll32.exe", vec!["powrprof.dll,SetSuspendState", "0,1,0"]),
            "shutdown" => ("shutdown", vec!["/s", "/t", "5"]),
            _ => return Err(format!("Unknown post-sync action: {kind}")),
        }
    };
    #[cfg(target_os = "macos")]
    let (prog, args): (&str, Vec<&str>) = {
        match kind.as_str() {
            "sleep" => ("pmset", vec!["sleepnow"]),
            "shutdown" => ("osascript", vec!["-e", "tell application \"System Events\" to shut down"]),
            _ => return Err(format!("Unknown post-sync action: {kind}")),
        }
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (prog, args): (&str, Vec<&str>) = {
        match kind.as_str() {
            "sleep" => ("systemctl", vec!["suspend"]),
            "shutdown" => ("systemctl", vec!["poweroff"]),
            _ => return Err(format!("Unknown post-sync action: {kind}")),
        }
    };
    std::process::Command::new(prog).args(&args).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Open (or focus) the standalone progress sub-window (Synchronize only; compare progress is shown inline in the main window).
/// **Must be an async command**: sync commands run on the main thread's IPC, while wry needs the main event loop
/// to pump messages in order to create a window — creating it synchronously leaves the sub-window's navigation stuck
/// on about:blank (an all-white window) and close events never get queued (it looks like "the whole app won't close").
/// An async command runs on its own thread, so window creation is proxied through the event loop correctly.
#[tauri::command]
pub async fn open_progress_window(
    app: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::RunState>>,
) -> Result<u64, String> {
    let st = state.inner().clone();
    let launch_id = crate::state::reserve_progress_launch(&st)?;
    match prepare_progress_window(&app, launch_id).await {
        Ok(()) => Ok(launch_id),
        Err(e) => {
            crate::state::release_progress_launch(&st, launch_id);
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

async fn prepare_progress_window(app: &tauri::AppHandle, launch_id: u64) -> Result<(), String> {
    use tauri::{Emitter, Listener, Manager};

    let window = if let Some(window) = app.get_webview_window("progress") {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        window
    } else {
        let (mounted_tx, mounted_rx) = std::sync::mpsc::sync_channel(1);
        let mounted_listener = app.once("progress-window-mounted", move |_| {
            let _ = mounted_tx.send(());
        });
        let built = tauri::WebviewWindowBuilder::new(app, "progress", tauri::WebviewUrl::App("progress.html".into()))
            .title("SyncDash — Run")
            .inner_size(620.0, 500.0)
            .min_inner_size(440.0, 380.0)
            .build()
            .map_err(|e| e.to_string());
        let window = match built {
            Ok(window) => window,
            Err(e) => {
                app.unlisten(mounted_listener);
                return Err(e);
            }
        };
        let mounted = wait_for_window_signal(
            mounted_rx,
            "Progress window did not load; synchronization was not started",
        )
        .await;
        app.unlisten(mounted_listener);
        if let Err(e) = mounted {
            let _ = window.destroy();
            return Err(e);
        }
        window
    };

    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let ready_event = format!("progress-window-ready-{launch_id}");
    let ready_listener = app.once(ready_event, move |_| {
        let _ = ready_tx.send(());
    });
    if let Err(e) = window.emit("progress-window-arm", launch_id) {
        app.unlisten(ready_listener);
        let _ = window.destroy();
        return Err(e.to_string());
    }
    let ready = wait_for_window_signal(
        ready_rx,
        "Progress window did not acknowledge this run; synchronization was not started",
    )
    .await;
    app.unlisten(ready_listener);
    if let Err(e) = ready {
        let _ = window.destroy();
        return Err(e);
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_progress_launch(
    state: tauri::State<'_, std::sync::Arc<crate::state::RunState>>,
    launch_id: u64,
) -> bool {
    crate::state::release_progress_launch(state.inner(), launch_id)
}

#[tauri::command]
pub fn close_progress_launch(
    state: tauri::State<'_, std::sync::Arc<crate::state::RunState>>,
) -> &'static str {
    crate::state::close_progress_launch(state.inner())
}

/// Destroy the progress sub-window. Not hide: a hidden sub-window keeps the process alive after the main window closes
#[tauri::command]
pub async fn close_progress_window(
    app: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::RunState>>,
) -> Result<(), String> {
    use tauri::Manager;
    let result = match app.get_webview_window("progress") {
        Some(w) => w.destroy().map_err(|e| e.to_string()),
        None => Ok(()),
    };
    crate::state::finish_progress_window_close(state.inner());
    result
}
