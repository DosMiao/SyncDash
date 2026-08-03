//! AutoScan commands. The main webview may arm or complete backend-owned work; it cannot invent a
//! different job revision/target and it never receives authority to write from this module.

use std::sync::Arc;

use syncdash::fs::vfs::spec::{parse, RootSpec};

use crate::features::autoscan::controller::AutoScanController;
use crate::features::autoscan::model::{AutoScanBinding, AutoScanStatusDto};
use crate::features::autoscan::worker::configuration::configured_interval;
use crate::features::operations::lifecycle::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub fn start_autoscan(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    controller: tauri::State<'_, Arc<AutoScanController>>,
    run_lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    expected_job_id: String,
    expected_revision: String,
    target_index: usize,
) -> Result<AutoScanStatusDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    // Cross the same short gate as Compare/Apply before reading the registry. A concurrent save
    // either finishes first or sees this command in flight, so it cannot leave a freshly armed
    // generation bound to the revision that existed just before its mutation.
    let _command = run_lifecycle.inner().command_lease()?;
    let (job_name, full_job) = syncdash::job::load_by_id(&expected_job_id).map_err(|error| {
        format!(
            "The selected job was deleted or replaced before AutoScan started — refresh it and try again: {error}"
        )
    })?;
    let revision = syncdash::job::config_revision(&full_job)
        .map_err(|error| format!("Job '{job_name}': {error}"))?;
    if revision != expected_revision {
        return Err(format!(
            "Job '{job_name}' changed before AutoScan started — refresh it and try again"
        ));
    }
    let selected_target = full_job.select_target(target_index)?;
    let local_roots = match (parse(&full_job.source), parse(selected_target.target())) {
        (RootSpec::Local(source), RootSpec::Local(target)) => Some((source, target)),
        _ => None,
    };
    controller.start(
        app,
        AutoScanBinding {
            job_id: full_job.job_id.clone(),
            job_name,
            config_revision: revision,
            target_index,
            interval_secs: configured_interval(&full_job),
            auto_apply: full_job.autoscan_auto_apply,
            rigor: full_job.rigor.clone(),
        },
        local_roots,
    )
}

#[tauri::command]
pub fn stop_autoscan(
    window: tauri::WebviewWindow,
    controller: tauri::State<'_, Arc<AutoScanController>>,
) -> Result<AutoScanStatusDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(controller.stop())
}

#[tauri::command]
pub fn autoscan_status(
    window: tauri::WebviewWindow,
    controller: tauri::State<'_, Arc<AutoScanController>>,
) -> Result<AutoScanStatusDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(controller.status())
}

#[tauri::command]
pub fn decline_autoscan_trigger(
    window: tauri::WebviewWindow,
    controller: tauri::State<'_, Arc<AutoScanController>>,
    generation: u64,
    ticket_id: u64,
) -> Result<AutoScanStatusDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    controller.decline_trigger(generation, ticket_id)
}
