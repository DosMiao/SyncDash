//! SyncDash desktop shell (Tauri v2): IPC orchestration only; all sync logic lives in the
//! syncdash core library.
//!
//! - `dto` — the wire types ts-rs exports to the frontend
//! - `bridge` — the typed progress event stream shared by both windows
//! - `state` — single-run mutual exclusion and the snapshot cache behind the "Identical" panel
//! - `cmd` — the commands themselves, grouped by what they act on
//!
//! Heavy work goes through `spawn_blocking`; window-creating commands must be `async fn`, because
//! a sync command runs on the main thread inside IPC and wry needs the event loop pumping to
//! finish creating a webview — a sync one deadlocks at about:blank and wedges close events behind it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod cmd;
mod dto;
mod state;

use std::sync::Arc;

use state::{RunState, SnapCache};

fn main() {
    // A windowed build has no console — the only home for diagnostics outside a run (settings parse
    // failures, pruning, migration) is app.jsonl. `_session` must live until the process exits.
    let _session = syncdash::boot::init(|cfg| {
        Some(Arc::new(syncdash::obs::logging::AppLogSink::open(&cfg.resolved_log_dir(), cfg.level)) as Arc<_>)
    });
    let cfg = &_session.settings;
    // Retention runs once at startup: the apply manifest records everything and grows without a gate
    let dropped = syncdash::obs::runlog::prune(cfg.keep_days, cfg.max_total_mb);
    if dropped > 0 {
        syncdash::log_info!("app", "Log cleanup: removed the records of {dropped} runs");
    }
    tauri::Builder::default()
        // Main window closes → cascade-destroy the progress sub-window; a leftover window keeps Tauri from exiting ("the app won't close")
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if window.label() == "main" {
                    use tauri::Manager;

                    if let Some(p) = window.app_handle().get_webview_window("progress") {
                        let _ = p.destroy();
                    }
                }
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(RunState::default()))
        .manage(Arc::new(SnapCache::default()))
        .invoke_handler(tauri::generate_handler![
            cmd::jobs::list_jobs, cmd::jobs::jobs_dir, cmd::jobs::get_job, cmd::jobs::default_job, cmd::jobs::job_file_schema, cmd::jobs::save_job, cmd::jobs::delete_job,
            cmd::edit::inspect_paths, cmd::edit::mask_match, cmd::edit::junk_presets,
            cmd::results::list_same, cmd::results::export_csv,
            cmd::logs::run_history, cmd::logs::last_syncs, cmd::logs::run_detail, cmd::logs::log_runs, cmd::logs::log_artifact, cmd::logs::log_dir_path, cmd::logs::app_log_tail, cmd::logs::get_settings, cmd::logs::save_settings,
            cmd::shell::reveal, cmd::shell::post_sync_action, cmd::shell::open_progress_window, cmd::shell::cancel_progress_launch, cmd::shell::close_progress_launch, cmd::shell::close_progress_window,
            cmd::run::compare_job, cmd::run::preflight, cmd::run::apply_job, cmd::run::cancel_run, cmd::run::pause_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running SyncDash");
}
