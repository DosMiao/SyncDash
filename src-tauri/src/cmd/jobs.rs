//! Job files: list, read, write, delete, and the schema the editor renders from.

use std::sync::Arc;

use syncdash::job::{self};
use syncdash::run;
use tauri::Emitter;

use crate::auth::AuthorizationStore;
use crate::autoscan::AutoScanController;
use crate::dto::{JobDeleteDto, JobDetailDto, JobDto, JobFileSchemaDto, JobSaveDto};
use crate::state::{with_run_idle, ResultRepository, RunState};

use super::require_main_window;

fn mutation_revokes_authority(effect: job::JobMutationEffect) -> bool {
    matches!(
        effect,
        job::JobMutationEffect::Updated | job::JobMutationEffect::Deleted
    )
}

#[tauri::command]
pub fn list_jobs() -> Result<Vec<JobDto>, String> {
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

#[tauri::command]
pub fn get_job(name: String) -> Result<JobDetailDto, String> {
    let (name, job) = job::load_named(&name).map_err(|e| e.to_string())?;
    let config_revision = job::config_revision(&job).map_err(|e| format!("Job '{name}': {e}"))?;
    Ok(JobDetailDto {
        job_id: job.job_id.clone(),
        name,
        job,
        config_revision,
    })
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
        .map(|on_disk| JobFileSchemaDto {
            on_disk,
            current: job::SCHEMA,
        })
        .map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn save_job(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    run_state: tauri::State<'_, Arc<RunState>>,
    results: tauri::State<'_, Arc<ResultRepository>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    authorizations: tauri::State<'_, Arc<AuthorizationStore>>,
    name: String,
    job: job::Job,
    original_name: Option<String>,
    expected_revision: Option<String>,
) -> Result<JobSaveDto, String> {
    require_main_window(&window)?;
    let (saved, autoscan_status) = with_run_idle(run_state.inner(), "Saving jobs", || {
        let saved = job::save_job(
            &name,
            &job,
            original_name.as_deref(),
            expected_revision.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        if mutation_revokes_authority(saved.effect) {
            authorizations.revoke_job(&saved.job_id);
        }
        let autoscan_status = match saved.effect {
            job::JobMutationEffect::Renamed
                if expected_revision.as_deref() == Some(saved.config_revision.as_str()) =>
            {
                autoscan.rebind_job_name(&saved.job_id, &saved.name)
            }
            job::JobMutationEffect::Renamed | job::JobMutationEffect::Updated => {
                autoscan.stop_if_job_id(&saved.job_id)
            }
            job::JobMutationEffect::Created | job::JobMutationEffect::NoOp => None,
            job::JobMutationEffect::Deleted => {
                unreachable!("save cannot produce a deleted outcome")
            }
        };
        Ok((saved, autoscan_status))
    })?;
    if let Some(status) = autoscan_status {
        let _ = app.emit("autoscan-status", status);
    }
    match saved.effect {
        job::JobMutationEffect::Renamed => {
            let mut repository = results.0.lock().unwrap();
            if expected_revision.as_deref() == Some(saved.config_revision.as_str()) {
                repository.rebind_job_name(&saved.job_id, &saved.name);
            } else if let Some(expected_revision) = expected_revision.as_deref() {
                repository.invalidate_revision(&saved.job_id, expected_revision);
            }
        }
        job::JobMutationEffect::Updated => {
            let expected_revision = expected_revision
                .as_deref()
                .expect("an updated job must carry its expected revision");
            if expected_revision != saved.config_revision {
                results
                    .0
                    .lock()
                    .unwrap()
                    .invalidate_revision(&saved.job_id, expected_revision);
            }
        }
        job::JobMutationEffect::Created | job::JobMutationEffect::NoOp => {}
        job::JobMutationEffect::Deleted => unreachable!("save cannot produce a deleted outcome"),
    }
    Ok(JobSaveDto {
        job_id: saved.job_id,
        name: saved.name,
        config_revision: saved.config_revision,
        effect: saved.effect,
        previous_name: saved.previous_name,
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn delete_job(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    run_state: tauri::State<'_, Arc<RunState>>,
    results: tauri::State<'_, Arc<ResultRepository>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    authorizations: tauri::State<'_, Arc<AuthorizationStore>>,
    name: String,
    expected_job_id: String,
    expected_revision: String,
) -> Result<JobDeleteDto, String> {
    require_main_window(&window)?;
    let (deleted, autoscan_status) = with_run_idle(run_state.inner(), "Deleting jobs", || {
        let deleted = job::delete_job(&name, &expected_job_id, &expected_revision)
            .map_err(|error| error.to_string())?;
        authorizations.revoke_job(&deleted.job_id);
        let autoscan_status = autoscan.stop_if_job_id(&deleted.job_id);
        Ok((deleted, autoscan_status))
    })?;
    if let Some(status) = autoscan_status {
        let _ = app.emit("autoscan-status", status);
    }
    results.0.lock().unwrap().invalidate_job(&deleted.job_id);
    Ok(JobDeleteDto {
        job_id: deleted.job_id,
        name: deleted.name,
        config_revision: deleted.config_revision,
        effect: deleted.effect,
    })
}

#[cfg(test)]
mod tests {
    use super::mutation_revokes_authority;
    use syncdash::job::JobMutationEffect;

    #[test]
    fn semantic_update_and_delete_revoke_but_rename_and_noop_preserve() {
        assert!(mutation_revokes_authority(JobMutationEffect::Updated));
        assert!(mutation_revokes_authority(JobMutationEffect::Deleted));
        assert!(!mutation_revokes_authority(JobMutationEffect::Renamed));
        assert!(!mutation_revokes_authority(JobMutationEffect::NoOp));
        assert!(!mutation_revokes_authority(JobMutationEffect::Created));
    }
}
