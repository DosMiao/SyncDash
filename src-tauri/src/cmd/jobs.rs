//! Job files: list, read, write, delete, and the schema the editor renders from.

use syncdash::job::{self};
use syncdash::run;

use crate::dto::{JobDto, JobFileSchemaDto};

#[tauri::command]
pub fn list_jobs() -> Vec<JobDto> {
    job::load_all()
        .into_iter()
        .map(|(name, j)| JobDto {
            name,
            mode: j.mode.clone(),
            rigor: j.rigor.clone(),
            source: j.source.clone(),
            target: j.target.clone(),
            has_archive: j.archive.is_some(),
            remote: run::is_peer_job(&j),
            remote_host: j.remote_host.clone(),
            versioning: j.versioning,
            delta: j.delta,
            parallel: j.parallel,
            include: j.include.clone(),
            exclude: j.exclude.clone(),
            watch_interval_secs: j.watch_interval_secs,
            watch_auto_apply: j.watch_auto_apply,
            targets: j.target_list(),
        })
        .collect()
}
#[tauri::command]
pub fn jobs_dir() -> String {
    syncdash::foundation::dirs::jobs_dir().display().to_string()
}
/// M5: the editor reads the full Job (the list_jobs DTO is only a summary)
#[tauri::command]
pub fn get_job(name: String) -> Result<job::Job, String> {
    job::load(&name).map(|(_, j)| j).map_err(|e| e.to_string())
}
/// What a brand-new job starts from, including the default-on junk presets already materialized into
/// `exclude`. The editor asks for this instead of keeping its own copy: a hand-written mirror of
/// `Job::default()` is a second source of truth for engine policy, and it had already drifted — an empty
/// exclude list where the engine seeds thirteen patterns, which would have created new jobs with no junk
/// protection at all.
#[tauri::command]
pub fn default_job() -> job::Job {
    job::Job::default()
}
#[tauri::command]
pub fn job_file_schema(name: String) -> Result<JobFileSchemaDto, String> {
    job::file_schema(&name)
        .map(|on_disk| JobFileSchemaDto { on_disk, current: job::SCHEMA })
        .map_err(|e| e.to_string())
}
/// M5: save a job (create, or overwrite the TOML of the same name)
#[tauri::command]
pub fn save_job(name: String, job: job::Job) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("Job name cannot be empty".into());
    }
    job::save_job(name.trim(), &job).map(|p| p.display().to_string()).map_err(|e| e.to_string())
}
/// M5: delete the job's config file (not a single byte of data is touched)
#[tauri::command]
pub fn delete_job(name: String) -> Result<(), String> {
    job::delete_job(&name).map_err(|e| e.to_string())
}

// P1: path health check and "show in file explorer"
