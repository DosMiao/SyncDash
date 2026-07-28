//! Run history, log artifacts, and application settings.


/// M4: run history (newest → oldest). job = null shows everything
#[tauri::command]
pub fn run_history(job: Option<String>, limit: Option<usize>) -> Vec<syncdash::obs::runlog::RunRecord> {
    syncdash::obs::runlog::history(job.as_deref(), limit.unwrap_or(50))
}
/// M4: the most recent run per job — the data behind the sidebar's "last sync" dot
#[tauri::command]
pub fn last_syncs() -> std::collections::HashMap<String, syncdash::obs::runlog::RunRecord> {
    syncdash::obs::runlog::latest_by_job()
}
/// M4: the detail lines of one run (raw JSONL; line count capped)
#[tauri::command]
pub fn run_detail(detail: String) -> Vec<String> {
    syncdash::obs::runlog::detail_lines(&detail, 2000)
}

// v0.10: centralized logging and app settings
/// The run list. Unlike `run_history`, this one also folds in **interrupted runs** (the ones missing
/// from the index that left only a directory behind) — the crashed run is exactly the one worth seeing.
#[tauri::command]
pub fn log_runs(job: Option<String>, limit: Option<usize>) -> Vec<syncdash::obs::runlog::RunRecord> {
    syncdash::obs::runlog::history_merged(job.as_deref(), limit.unwrap_or(100))
}
/// One artifact of a run (which ∈ run / errors / items / plan / summary).
/// Line count capped: the apply manifest records everything, one large sync runs to tens of thousands of lines, and shipping all of it over IPC would freeze the UI.
#[tauri::command]
pub fn log_artifact(run_id: String, which: String, max: Option<usize>) -> Vec<String> {
    syncdash::obs::runlog::artifact_lines(&run_id, &which, max.unwrap_or(5000))
}
/// The log root directory (the "open folder" button hands it to the existing `reveal`)
#[tauri::command]
pub fn log_dir_path(run_id: Option<String>) -> String {
    let root = syncdash::obs::runlog::logs_dir();
    match run_id {
        Some(id) if !id.is_empty() => root.join(id).display().to_string(),
        _ => root.display().to_string(),
    }
}
/// Events outside of a run (startup, settings errors, prune, migration). Returns the last n lines.
#[tauri::command]
pub fn app_log_tail(n: Option<usize>) -> Vec<String> {
    let n = n.unwrap_or(500);
    let p = syncdash::obs::runlog::logs_dir().join(syncdash::foundation::names::APP_LOG_FILE);
    let Ok(text) = std::fs::read_to_string(p) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines.iter().rev().take(n).rev().map(|s| s.to_string()).collect()
}
#[tauri::command]
pub fn get_settings() -> syncdash::store::settings::AppSettings {
    syncdash::store::settings::load()
}
/// Save settings. `migrate` = move the whole old directory over when the log directory changes.
///
/// The old location must be resolved **before** the new config is written — ask afterwards and you only get the new value.
#[tauri::command]
pub fn save_settings(
    s: syncdash::store::settings::AppSettings,
    migrate: bool,
) -> Result<syncdash::store::migrate::MigrateReport, String> {
    let old_dir = syncdash::store::settings::load().wanted_log_dir();
    let new_dir = s.wanted_log_dir();
    syncdash::store::settings::save(&s).map_err(|e| e.to_string())?;
    if migrate && old_dir != new_dir {
        Ok(syncdash::store::migrate::migrate_log_dir(&old_dir, &new_dir))
    } else {
        Ok(syncdash::store::migrate::MigrateReport::default())
    }
}
