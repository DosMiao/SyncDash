use std::sync::Arc;

use syncdash::job;

use crate::contracts::compare::{
    CompareIdentity, CompareResultForgetDto, CompareWorkspaceLookupDto,
};
use crate::features::compare::evidence::repository::{
    CompareResultForgetOutcome, CompareResultRepository,
};
use crate::features::compare::workspace;
use crate::features::jobs::target::resolve_target;
use crate::features::operations::lifecycle::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub fn reconcile_compare_workspace(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    compare_identity: CompareIdentity,
) -> Result<CompareWorkspaceLookupDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    let _command = lifecycle.inner().command_lease()?;
    let job_state = workspace::job_state_for(&compare_identity)?;
    results
        .reconcile_exact_workspace(&compare_identity, job_state)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn restore_compare(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    job_id: String,
    target_index: usize,
    expected_config_revision: String,
) -> Result<CompareWorkspaceLookupDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    let _command = lifecycle.inner().command_lease()?;
    let (job_name, full_job) = job::load_by_id(&job_id).map_err(|e| e.to_string())?;
    let current_config_revision =
        job::config_revision(&full_job).map_err(|e| format!("Job '{job_name}': {e}"))?;
    workspace::require_expected_config_revision(
        &job_name,
        &expected_config_revision,
        &current_config_revision,
    )?;
    let (resolved_target_index, _) = resolve_target(&full_job, Some(target_index))?;
    results
        .rebind_job_name(&full_job.job_id, &job_name)
        .map_err(|error| error.to_string())?;
    results
        .restore_workspace(
            &full_job.job_id,
            resolved_target_index,
            &expected_config_revision,
        )
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn forget_compare_result(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    compare_identity: CompareIdentity,
) -> Result<CompareResultForgetDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    let _command = lifecycle.inner().command_lease()?;
    Ok(
        match results
            .forget(&compare_identity)
            .map_err(|error| error.to_string())?
        {
            CompareResultForgetOutcome::Forgotten { cleanup_warning } => {
                CompareResultForgetDto::Forgotten { cleanup_warning }
            }
            CompareResultForgetOutcome::AlreadyForgotten => {
                CompareResultForgetDto::AlreadyForgotten
            }
        },
    )
}
