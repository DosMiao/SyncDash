//! SyncDash desktop shell (Tauri v2): IPC orchestration only; all sync logic lives in the
//! syncdash core library.
//!
//! - `dto` — the wire types ts-rs exports to the frontend
//! - `bridge` — the typed progress event stream shared by both windows
//! - `compare_results` — exact, versioned retention for successful Compare results
//! - `operation_authorization` — typed review challenges and one-use operation authority
//! - `job_target` — exact multi-target job resolution
//! - `operation_decisions` — executable-operation reconstruction from reviewed decisions
//! - `run_lifecycle` — active runs, command preparation, and progress-launch reservations
//! - `cmd` — the commands themselves, grouped by what they act on
//!
//! Heavy work goes through `spawn_blocking`; window-creating commands must be `async fn`, because
//! a sync command runs on the main thread inside IPC and wry needs the event loop pumping to
//! finish creating a webview — a sync one deadlocks at about:blank and wedges close events behind it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod authority_token;
mod autoscan;
mod bridge;
mod cmd;
mod compare_results;
mod csv_export;
mod csv_export_receipts;
mod dto;
mod job_target;
mod operation_authorization;
mod operation_decisions;
mod run_lifecycle;
mod settings_authority;
mod window_role;

use std::sync::Arc;

use autoscan::AutoScanController;
use bridge::RunEventRepository;
use compare_results::CompareResultRepository;
use csv_export_receipts::CsvExportReceiptRepository;
use operation_authorization::OperationAuthorizationStore;
use run_lifecycle::RunLifecycle;
use settings_authority::SettingsAuthority;
use window_role::{MAIN_WINDOW_LABEL, PROGRESS_WINDOW_LABEL};

fn main() {
    // A windowed build has no console — the only home for diagnostics outside a run (settings parse
    // failures, pruning, migration) is app.jsonl. `_session` must live until the process exits.
    let mut app_log = None;
    let _session = syncdash::boot::init(|cfg| {
        let sink = Arc::new(syncdash::obs::logging::AppLogSink::open(
            &cfg.resolved_log_dir(),
            cfg.level,
        ));
        app_log = Some(sink.clone());
        Some(sink as Arc<_>)
    });
    let app_log = app_log.expect("desktop startup must construct an application log sink");
    let cfg = &_session.settings;
    // Retention runs once at startup: the apply manifest records everything and grows without a gate
    match syncdash::obs::runlog::prune(cfg.keep_days, cfg.max_total_mb) {
        Ok(dropped) if dropped > 0 => {
            syncdash::log_info!("app", "Log cleanup: removed the records of {dropped} runs");
        }
        Ok(_) => {}
        Err(error) => syncdash::log_error!("app", "Log cleanup failed: {error}"),
    }
    let lifecycle = Arc::new(RunLifecycle::default());
    let authorizations = Arc::new(OperationAuthorizationStore::default());
    let results = Arc::new(
        CompareResultRepository::open_default()
            .expect("durable Compare-result repository must pass integrity validation"),
    );
    let autoscan = Arc::new(AutoScanController::new(
        results.clone(),
        authorizations.clone(),
    ));
    tauri::Builder::default()
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == MAIN_WINDOW_LABEL {
                    use tauri::{Emitter, Manager};

                    let lifecycle = window.state::<Arc<RunLifecycle>>();
                    if lifecycle.has_activity() {
                        api.prevent_close();
                        let _ = window.emit(
                            "main-close-blocked",
                            "A compare or synchronization is still running. Cancel it before closing SyncDash.",
                        );
                        let _ = window.show();
                        let _ = window.set_focus();
                        if let Some(progress) = window
                            .app_handle()
                            .get_webview_window(PROGRESS_WINDOW_LABEL)
                        {
                            let _ = progress.show();
                            let _ = progress.set_focus();
                        }
                        return;
                    }

                    window.state::<Arc<AutoScanController>>().stop();

                    if let Some(p) = window
                        .app_handle()
                        .get_webview_window(PROGRESS_WINDOW_LABEL)
                    {
                        let _ = p.destroy();
                    }
                }
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .manage(lifecycle)
        .manage(authorizations)
        .manage(autoscan)
        .manage(results)
        .manage(Arc::new(CsvExportReceiptRepository::default()))
        .manage(Arc::new(SettingsAuthority::default()))
        .manage(Arc::new(RunEventRepository::default()))
        .manage(app_log)
        .invoke_handler(tauri::generate_handler![
            cmd::jobs::list_jobs, cmd::jobs::jobs_dir, cmd::jobs::get_job, cmd::jobs::default_job, cmd::jobs::job_file_schema, cmd::jobs::save_job, cmd::jobs::update_job_root, cmd::jobs::swap_job_roots, cmd::jobs::delete_job,
            cmd::autoscan::start_autoscan, cmd::autoscan::stop_autoscan, cmd::autoscan::autoscan_status, cmd::autoscan::decline_autoscan_trigger,
            cmd::edit::inspect_paths, cmd::edit::mask_match, cmd::edit::junk_presets,
            cmd::results::reconcile_compare_workspace, cmd::results::restore_compare, cmd::results::forget_compare_result, cmd::results::list_identical, cmd::results::export_compare_csv, cmd::results::reveal_compare_row, cmd::results::reveal_csv_export,
            cmd::logs::latest_run_records, cmd::logs::log_runs, cmd::logs::log_artifact, cmd::logs::reveal_log_location, cmd::logs::get_settings, cmd::logs::pick_log_directory, cmd::logs::save_settings,
            cmd::shell::open_progress_window, cmd::shell::cancel_progress_launch, cmd::shell::report_progress_window_mounted, cmd::shell::acknowledge_progress_launch, cmd::shell::execute_post_run_power_action, cmd::shell::begin_progress_window_close, cmd::shell::destroy_progress_window,
            cmd::run::review_compare, cmd::run::approve_operation, cmd::run::compare_job, cmd::run::review_apply, cmd::run::authorize_autoscan_apply, cmd::run::apply_job, cmd::run::replay_compare_events, cmd::run::replay_apply_events, cmd::run::cancel_compare_run, cmd::run::cancel_apply_run, cmd::run::set_apply_paused
        ])
        .run(tauri::generate_context!())
        .expect("error while running SyncDash");
}
