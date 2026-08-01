//! Job files: list, read, write, delete, and the schema the editor renders from.

use syncdash::job::{self};
use syncdash::run;

use crate::dto::{JobDto, JobFileSchemaDto};

#[tauri::command]
pub fn list_jobs() -> Result<Vec<JobDto>, String> {
    job::load_all()
        .into_iter()
        .map(|(name, j)| {
            let config_revision = job::config_revision(&j).map_err(|e| format!("Job '{name}': {e}"))?;
            Ok(JobDto {
                name,
                config_revision,
                mode: j.mode.clone(),
                rigor: j.rigor.clone(),
                source: j.source.clone(),
                target: j.target.clone(),
                has_archive: j.archive.is_some(),
                remote: run::is_peer_job(&j),
                versioning: j.versioning,
                delta: j.delta,
                parallel: j.parallel,
                include: j.include.clone(),
                exclude: j.exclude.clone(),
                watch_interval_secs: j.watch_interval_secs,
                watch_auto_apply: j.watch_auto_apply,
                targets: j.target_list(),
            })
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
    job::load_named(&name).map(|(_, j)| j).map_err(|e| e.to_string())
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
    job::file_schema_named(&name)
        .map(|on_disk| JobFileSchemaDto { on_disk, current: job::SCHEMA })
        .map_err(|e| e.to_string())
}

/// M5: save a job (create, or overwrite the TOML of the same name)
#[tauri::command]
pub fn save_job(name: String, job: job::Job) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("Job name cannot be empty".into());
    }
    let config_revision = job::config_revision(&job).map_err(|e| format!("Job '{name}': {e}"))?;
    job::save_job(&name, &job).map_err(|e| e.to_string())?;
    Ok(config_revision)
}

/// M5: delete the job's config file (not a single byte of data is touched)
#[tauri::command]
pub fn delete_job(name: String) -> Result<(), String> {
    job::delete_job(&name).map_err(|e| e.to_string())
}

// P1: path health check and "show in file explorer"
