//! SyncDash desktop composition root.
//!
//! This is the only module that constructs concrete feature state and registers IPC commands.

use std::sync::Arc;

use crate::features::autoscan::controller::AutoScanController;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::compare::export::receipt::CsvExportReceiptRepository;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::events::repository::RunEventRepository;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;
use crate::features::settings::authorization::grant::SettingsAuthority;
use crate::window::{MAIN_WINDOW_LABEL, PROGRESS_WINDOW_LABEL};

pub(crate) fn run() {
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
    match syncdash::run::history::prune(cfg.keep_days, cfg.max_total_mb) {
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
            crate::ipc::commands::jobs::query::list_jobs, crate::ipc::commands::jobs::query::jobs_dir, crate::ipc::commands::jobs::query::get_job, crate::ipc::commands::jobs::query::default_job, crate::ipc::commands::jobs::query::job_file_schema, crate::ipc::commands::jobs::mutation::save_job, crate::ipc::commands::jobs::mutation::update_job_root, crate::ipc::commands::jobs::mutation::swap_job_roots, crate::ipc::commands::jobs::mutation::delete_job,
            crate::ipc::commands::autoscan::start_autoscan, crate::ipc::commands::autoscan::stop_autoscan, crate::ipc::commands::autoscan::autoscan_status, crate::ipc::commands::autoscan::decline_autoscan_trigger,
            crate::ipc::commands::job_editor::inspect_paths, crate::ipc::commands::job_editor::mask_match, crate::ipc::commands::job_editor::junk_presets,
            crate::ipc::commands::compare::workspace::reconcile_compare_workspace, crate::ipc::commands::compare::workspace::restore_compare, crate::ipc::commands::compare::workspace::forget_compare_result, crate::ipc::commands::compare::identical::list_identical, crate::ipc::commands::compare::export::export_compare_csv, crate::ipc::commands::compare::reveal::reveal_compare_row, crate::ipc::commands::compare::reveal::reveal_csv_export,
            crate::ipc::commands::logs::query::latest_run_records, crate::ipc::commands::logs::query::log_runs, crate::ipc::commands::logs::query::log_artifact, crate::ipc::commands::logs::reveal::reveal_log_location,
            crate::ipc::commands::settings::query::get_settings, crate::ipc::commands::settings::selection::pick_log_directory, crate::ipc::commands::settings::save::save_settings,
            crate::ipc::commands::desktop::progress_window::open_progress_window, crate::ipc::commands::desktop::progress_window::cancel_progress_launch, crate::ipc::commands::desktop::progress_window::report_progress_window_mounted, crate::ipc::commands::desktop::progress_window::acknowledge_progress_launch, crate::ipc::commands::desktop::power::execute_post_run_power_action, crate::ipc::commands::desktop::progress_window::begin_progress_window_close, crate::ipc::commands::desktop::progress_window::destroy_progress_window,
            crate::ipc::commands::operations::compare::review::review_compare, crate::ipc::commands::operations::compare::approval::approve_operation, crate::ipc::commands::operations::compare::launch::compare_job,
            crate::ipc::commands::operations::apply::review::review_apply, crate::ipc::commands::operations::apply::review::authorize_autoscan_apply, crate::ipc::commands::operations::apply::launch::apply_job,
            crate::ipc::commands::operations::control::replay_compare_events, crate::ipc::commands::operations::control::replay_apply_events, crate::ipc::commands::operations::control::cancel_compare_run, crate::ipc::commands::operations::control::cancel_apply_run, crate::ipc::commands::operations::control::set_apply_paused
        ])
        .run(tauri::generate_context!())
        .expect("error while running SyncDash");
}
