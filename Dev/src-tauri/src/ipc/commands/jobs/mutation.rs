use std::sync::Arc;

use syncdash::job;

use crate::contracts::jobs::{JobDeleteDto, JobRootMutationDto, JobSaveDto};
use crate::features::autoscan::controller::AutoScanController;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::jobs::mutation::effects::{
    apply_saved_job_side_effects, delete_job_side_effects,
};
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

use super::delivery::deliver_statuses;
use crate::features::jobs::mutation::projection::job_save_dto;

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
    let status_delivery_warnings = deliver_statuses(app, &events);
    Ok(JobRootMutationDto {
        mutation: job_save_dto(root_mutation.mutation, status_delivery_warnings),
        source: root_mutation.source,
        targets: root_mutation.targets,
    })
}

#[allow(clippy::too_many_arguments)]
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
    Ok(job_save_dto(saved, deliver_statuses(&app, &events)))
}

#[allow(clippy::too_many_arguments)]
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
        &run_lifecycle,
        &results,
        &autoscan,
        &authorizations,
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

#[allow(clippy::too_many_arguments)]
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
        &run_lifecycle,
        &results,
        &autoscan,
        &authorizations,
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

#[allow(clippy::too_many_arguments)]
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
        let events = delete_job_side_effects(&deleted.job_id, &results, &autoscan, &authorizations);
        Ok((deleted, events))
    })?;
    Ok(JobDeleteDto {
        job_id: deleted.job_id,
        name: deleted.name,
        config_revision: deleted.config_revision,
        effect: deleted.effect,
        status_delivery_warnings: deliver_statuses(&app, &events),
    })
}
