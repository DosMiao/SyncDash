use crate::ipc::{require_window_role, WindowRole};

const RUN_HISTORY_LIMIT: usize = 100;
const LOG_ARTIFACT_LINE_LIMIT: usize = 5_000;

#[tauri::command]
pub fn latest_run_records(
    window: tauri::WebviewWindow,
) -> Result<Vec<syncdash::run::history::LatestRunRecord>, String> {
    require_window_role(&window, WindowRole::Main)?;
    syncdash::run::history::latest_by_job().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn log_runs(
    window: tauri::WebviewWindow,
    job_id: Option<String>,
) -> Result<Vec<syncdash::run::history::RunRecord>, String> {
    require_window_role(&window, WindowRole::Main)?;
    syncdash::run::history::history_merged_for_registered_job(job_id.as_deref(), RUN_HISTORY_LIMIT)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn log_artifact(
    window: tauri::WebviewWindow,
    record_id: String,
    artifact: syncdash::run::history::LogArtifactKind,
) -> Result<Vec<String>, String> {
    require_window_role(&window, WindowRole::Main)?;
    syncdash::run::history::artifact_lines(&record_id, artifact, LOG_ARTIFACT_LINE_LIMIT)
}
