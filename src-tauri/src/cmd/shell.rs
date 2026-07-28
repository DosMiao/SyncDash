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
    let (prog, args): (&str, Vec<&str>) = if cfg!(windows) {
        match kind.as_str() {
            "sleep" => ("rundll32.exe", vec!["powrprof.dll,SetSuspendState", "0,1,0"]),
            "shutdown" => ("shutdown", vec!["/s", "/t", "5"]),
            _ => return Ok(()),
        }
    } else {
        match kind.as_str() {
            "sleep" => ("pmset", vec!["sleepnow"]),
            "shutdown" => ("osascript", vec!["-e", "tell application \"System Events\" to shut down"]),
            _ => return Ok(()),
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
pub async fn open_progress_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("progress") {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(&app, "progress", tauri::WebviewUrl::App("progress.html".into()))
        .title("SyncDash — Run")
        .inner_size(620.0, 500.0)
        .min_inner_size(440.0, 380.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Destroy the progress sub-window. Not hide: a hidden sub-window keeps the process alive after the main window closes
#[tauri::command]
pub async fn close_progress_window(app: tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("progress") {
        let _ = w.destroy();
    }
}
