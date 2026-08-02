use syncdash::job::{self};
use syncdash::run;

use crate::contracts::jobs::{JobDetailDto, JobDto, JobFileSchemaDto};
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub fn list_jobs(window: tauri::WebviewWindow) -> Result<Vec<JobDto>, String> {
    require_window_role(&window, WindowRole::Main)?;
    job::load_all()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|(name, j)| {
            let config_revision =
                job::config_revision(&j).map_err(|e| format!("Job '{name}': {e}"))?;
            Ok(JobDto {
                job_id: j.job_id.clone(),
                name,
                config_revision,
                mode: j.mode.clone(),
                rigor: j.rigor.clone(),
                source: j.source.clone(),
                has_archive: j.archive.is_some(),
                is_peer_job: run::is_peer_job(&j),
                versioning: j.versioning,
                delta: j.delta,
                parallel: j.parallel,
                include: j.include.clone(),
                exclude: j.exclude.clone(),
                autoscan_interval_secs: j.autoscan_interval_secs,
                autoscan_auto_apply: j.autoscan_auto_apply,
                targets: j.targets.clone(),
            })
        })
        .collect()
}

#[tauri::command]
pub fn jobs_dir(window: tauri::WebviewWindow) -> Result<String, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(syncdash::foundation::dirs::jobs_dir().display().to_string())
}

#[tauri::command]
pub fn get_job(window: tauri::WebviewWindow, name: String) -> Result<JobDetailDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    let (name, job) = job::load_named(&name).map_err(|e| e.to_string())?;
    let config_revision = job::config_revision(&job).map_err(|e| format!("Job '{name}': {e}"))?;
    Ok(JobDetailDto {
        job_id: job.job_id.clone(),
        name,
        job,
        config_revision,
    })
}

#[tauri::command]
pub fn default_job(window: tauri::WebviewWindow) -> Result<job::Job, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(job::Job::default())
}

#[tauri::command]
pub fn job_file_schema(
    window: tauri::WebviewWindow,
    name: String,
) -> Result<JobFileSchemaDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    job::file_schema_named(&name)
        .map(|on_disk| JobFileSchemaDto {
            on_disk,
            current: job::SCHEMA,
        })
        .map_err(|e| e.to_string())
}
