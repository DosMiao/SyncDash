//! Job files: list, read, write, delete, and the schema the editor renders from.

use std::sync::Arc;

use syncdash::job::{self};
use syncdash::run;
use tauri::Emitter;

use crate::autoscan::{AutoScanController, AutoScanStatusDto};
use crate::compare_results::CompareResultRepository;
use crate::dto::{
    CompareScopeExecutionStatusDto, JobDeleteDto, JobDetailDto, JobDto, JobFileSchemaDto,
    JobRootMutationDto, JobSaveDto,
};
use crate::operation_authorization::OperationAuthorizationStore;
use crate::run_lifecycle::RunLifecycle;
use crate::window_role::{require_window_role, WindowRole, MAIN_WINDOW_LABEL};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SavedJobMutationFacts {
    renamed: bool,
    configuration_changed: bool,
}

struct JobMutationStatusEvents {
    autoscan: Option<AutoScanStatusDto>,
    compare_execution: Vec<CompareScopeExecutionStatusDto>,
}

fn saved_job_mutation_facts(
    saved: &job::SavedJob,
    expected_revision: Option<&str>,
) -> SavedJobMutationFacts {
    SavedJobMutationFacts {
        renamed: saved.previous_name.is_some(),
        configuration_changed: expected_revision
            .is_some_and(|revision| revision != saved.config_revision),
    }
}

fn apply_saved_job_side_effects(
    saved: &job::SavedJob,
    expected_revision: Option<&str>,
    results: &CompareResultRepository,
    autoscan: &AutoScanController,
    authorizations: &OperationAuthorizationStore,
) -> Result<JobMutationStatusEvents, String> {
    let mutation = saved_job_mutation_facts(saved, expected_revision);
    if mutation.renamed {
        results
            .rebind_job_name(&saved.job_id, &saved.name)
            .map_err(|error| error.to_string())?;
    }
    if mutation.configuration_changed {
        authorizations.revoke_job_authority(&saved.job_id);
    }
    let rename_status = if mutation.renamed {
        autoscan.rebind_job_name(&saved.job_id, &saved.name)
    } else {
        None
    };
    let autoscan = if mutation.configuration_changed {
        autoscan.stop_if_job_id(&saved.job_id)
    } else {
        rename_status
    };
    let compare_execution = if mutation.configuration_changed {
        let previous_revision =
            expected_revision.expect("a semantic job update must carry its previous revision");
        results.expire_revision(
            &saved.job_id,
            previous_revision,
            crate::dto::CompareExecutionExpiryReasonDto::JobChanged,
        )
    } else {
        Vec::new()
    };
    Ok(JobMutationStatusEvents {
        autoscan,
        compare_execution,
    })
}

fn deliver_job_mutation_statuses(
    app: &tauri::AppHandle,
    events: &JobMutationStatusEvents,
) -> Vec<String> {
    let mut failures = Vec::new();
    if let Some(status) = &events.autoscan {
        if let Err(error) = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", status) {
            failures.push(format!("autoscan-status: {error}"));
        }
    }
    for status in &events.compare_execution {
        if let Err(error) = app.emit_to(MAIN_WINDOW_LABEL, "compare-execution-status", status) {
            failures.push(format!("compare-execution-status: {error}"));
        }
    }
    failures
}

fn job_save_dto(saved: job::SavedJob, status_delivery_warnings: Vec<String>) -> JobSaveDto {
    JobSaveDto {
        job_id: saved.job_id,
        name: saved.name,
        config_revision: saved.config_revision,
        effect: saved.effect,
        previous_name: saved.previous_name,
        status_delivery_warnings,
    }
}

fn execute_job_root_mutation<F>(
    app: &tauri::AppHandle,
    run_lifecycle: &RunLifecycle,
    results: &CompareResultRepository,
    autoscan: &AutoScanController,
    authorizations: &OperationAuthorizationStore,
    expected_config_revision: &str,
    mutate: F,
) -> Result<JobRootMutationDto, String>
where
    F: FnOnce() -> std::io::Result<job::JobRootMutation>,
{
    let (root_mutation, events) = run_lifecycle.with_idle_mutation("Saving a job root", || {
        let root_mutation = mutate().map_err(|error| error.to_string())?;
        let events = apply_saved_job_side_effects(
            &root_mutation.mutation,
            Some(expected_config_revision),
            results,
            autoscan,
            authorizations,
        )?;
        Ok((root_mutation, events))
    })?;
    let status_delivery_warnings = deliver_job_mutation_statuses(app, &events);
    Ok(JobRootMutationDto {
        mutation: job_save_dto(root_mutation.mutation, status_delivery_warnings),
        source: root_mutation.source,
        targets: root_mutation.targets,
    })
}

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

/// What a brand-new job starts from, including the default-on junk presets already materialized into
/// `exclude`. The editor asks for this instead of keeping its own copy: a hand-written mirror of
/// `Job::default()` is a second source of truth for engine policy, and it had already drifted — an empty
/// exclude list where the engine seeds thirteen patterns, which would have created new jobs with no junk
/// protection at all.
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

#[allow(clippy::too_many_arguments)] // Tauri injects state and exposes the rest as flat IPC fields.
#[tauri::command]
pub fn save_job(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    run_lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    name: String,
    job: job::Job,
    original_name: Option<String>,
    expected_revision: Option<String>,
) -> Result<JobSaveDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    let (saved, events) = run_lifecycle.with_idle_mutation("Saving jobs", || {
        let saved = job::save_job(
            &name,
            &job,
            original_name.as_deref(),
            expected_revision.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        let events = apply_saved_job_side_effects(
            &saved,
            expected_revision.as_deref(),
            &results,
            &autoscan,
            &authorizations,
        )?;
        Ok((saved, events))
    })?;
    let status_delivery_warnings = deliver_job_mutation_statuses(&app, &events);
    Ok(job_save_dto(saved, status_delivery_warnings))
}

#[allow(clippy::too_many_arguments)] // Tauri injects state and exposes the rest as flat IPC fields.
#[tauri::command]
pub fn update_job_root(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    run_lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    name: String,
    expected_job_id: String,
    expected_config_revision: String,
    target_index: usize,
    field: job::JobRootField,
    value: String,
) -> Result<JobRootMutationDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    execute_job_root_mutation(
        &app,
        run_lifecycle.inner().as_ref(),
        results.inner().as_ref(),
        autoscan.inner().as_ref(),
        authorizations.inner().as_ref(),
        &expected_config_revision,
        || {
            job::update_job_root(
                &name,
                &expected_job_id,
                &expected_config_revision,
                target_index,
                field,
                &value,
            )
        },
    )
}

#[allow(clippy::too_many_arguments)] // Tauri injects state and exposes the rest as flat IPC fields.
#[tauri::command]
pub fn swap_job_roots(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    run_lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    name: String,
    expected_job_id: String,
    expected_config_revision: String,
    target_index: usize,
) -> Result<JobRootMutationDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    execute_job_root_mutation(
        &app,
        run_lifecycle.inner().as_ref(),
        results.inner().as_ref(),
        autoscan.inner().as_ref(),
        authorizations.inner().as_ref(),
        &expected_config_revision,
        || {
            job::swap_job_roots(
                &name,
                &expected_job_id,
                &expected_config_revision,
                target_index,
            )
        },
    )
}

#[allow(clippy::too_many_arguments)] // Tauri injects state and exposes the rest as flat IPC fields.
#[tauri::command]
pub fn delete_job(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    run_lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    name: String,
    expected_job_id: String,
    expected_revision: String,
) -> Result<JobDeleteDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    let (deleted, events) = run_lifecycle.with_idle_mutation("Deleting jobs", || {
        let deleted = job::delete_job(&name, &expected_job_id, &expected_revision)
            .map_err(|error| error.to_string())?;
        authorizations.revoke_job_authority(&deleted.job_id);
        let events = JobMutationStatusEvents {
            autoscan: autoscan.stop_if_job_id(&deleted.job_id),
            compare_execution: results.expire_job(&deleted.job_id),
        };
        Ok((deleted, events))
    })?;
    let status_delivery_warnings = deliver_job_mutation_statuses(&app, &events);
    Ok(JobDeleteDto {
        job_id: deleted.job_id,
        name: deleted.name,
        config_revision: deleted.config_revision,
        effect: deleted.effect,
        status_delivery_warnings,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{job_save_dto, saved_job_mutation_facts, SavedJobMutationFacts};
    use syncdash::job::{JobMutationEffect, SavedJob};

    fn saved_job(
        effect: JobMutationEffect,
        config_revision: &str,
        previous_name: Option<&str>,
    ) -> SavedJob {
        SavedJob {
            name: "archive".into(),
            path: PathBuf::from("archive.toml"),
            job_id: "job-a".into(),
            config_revision: config_revision.into(),
            effect,
            previous_name: previous_name.map(str::to_string),
        }
    }

    #[test]
    fn authority_tracks_revision_change_independently_from_rename() {
        let renamed_and_changed =
            saved_job(JobMutationEffect::Renamed, "revision-b", Some("photos"));
        assert_eq!(
            saved_job_mutation_facts(&renamed_and_changed, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: true,
                configuration_changed: true,
            }
        );

        let pure_rename = saved_job(JobMutationEffect::Renamed, "revision-a", Some("photos"));
        assert_eq!(
            saved_job_mutation_facts(&pure_rename, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: true,
                configuration_changed: false,
            }
        );

        let semantic_update = saved_job(JobMutationEffect::Updated, "revision-b", None);
        assert_eq!(
            saved_job_mutation_facts(&semantic_update, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: false,
                configuration_changed: true,
            }
        );

        let no_op = saved_job(JobMutationEffect::NoOp, "revision-a", None);
        assert_eq!(
            saved_job_mutation_facts(&no_op, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: false,
                configuration_changed: false,
            }
        );

        let schema_only_update = saved_job(JobMutationEffect::Updated, "revision-a", None);
        assert_eq!(
            saved_job_mutation_facts(&schema_only_update, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: false,
                configuration_changed: false,
            }
        );
    }

    #[test]
    fn committed_save_response_preserves_status_delivery_warnings() {
        let response = job_save_dto(
            saved_job(JobMutationEffect::Updated, "revision-b", None),
            vec!["compare-execution-status: listener unavailable".into()],
        );
        assert_eq!(response.effect, JobMutationEffect::Updated);
        assert_eq!(
            response.status_delivery_warnings,
            vec!["compare-execution-status: listener unavailable"]
        );
    }
}
