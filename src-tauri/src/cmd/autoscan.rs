//! AutoScan commands. The main webview may arm or complete backend-owned work; it cannot invent a
//! different job revision/target and it never receives authority to write from this module.

use std::path::PathBuf;
use std::sync::Arc;

use syncdash::fs::vfs::spec::{parse, RootSpec};

use crate::autoscan::{
    configured_interval, AutoScanBinding, AutoScanController, AutoScanStatusDto,
};
use crate::dto::CompareOwner;

fn require_main(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("AutoScan can only be controlled from the main window".into())
    }
}

#[tauri::command]
pub fn start_autoscan(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    controller: tauri::State<'_, Arc<AutoScanController>>,
    name: String,
    expected_job_id: String,
    expected_revision: String,
    target_index: usize,
) -> Result<AutoScanStatusDto, String> {
    require_main(&window)?;
    let (job_name, full_job) =
        syncdash::job::load_named(&name).map_err(|error| error.to_string())?;
    if full_job.job_id != expected_job_id {
        return Err(format!(
            "Job '{job_name}' was replaced before AutoScan started — refresh it and try again"
        ));
    }
    let revision = syncdash::job::config_revision(&full_job)
        .map_err(|error| format!("Job '{job_name}': {error}"))?;
    if revision != expected_revision {
        return Err(format!(
            "Job '{job_name}' changed before AutoScan started — refresh it and try again"
        ));
    }
    let target = full_job
        .target_list()
        .get(target_index)
        .cloned()
        .ok_or_else(|| format!("Job '{job_name}' has no target {}", target_index + 1))?;
    let job = full_job.for_target(&target);
    job.validate()?;
    let local_roots = match (parse(&job.source), parse(&job.target)) {
        (RootSpec::Local(source), RootSpec::Local(target)) => {
            Some((PathBuf::from(source), PathBuf::from(target)))
        }
        _ => None,
    };
    Ok(controller.start(
        app,
        AutoScanBinding {
            job_id: full_job.job_id.clone(),
            job_name,
            config_revision: revision,
            target_index,
            interval_secs: configured_interval(&job),
            auto_apply: job.watch_auto_apply,
            rigor: job.rigor.clone(),
        },
        local_roots,
    ))
}

#[tauri::command]
pub fn stop_autoscan(
    window: tauri::WebviewWindow,
    controller: tauri::State<'_, Arc<AutoScanController>>,
) -> Result<AutoScanStatusDto, String> {
    require_main(&window)?;
    Ok(controller.stop())
}

#[tauri::command]
pub fn autoscan_status(
    window: tauri::WebviewWindow,
    controller: tauri::State<'_, Arc<AutoScanController>>,
) -> Result<AutoScanStatusDto, String> {
    require_main(&window)?;
    Ok(controller.status())
}

#[tauri::command]
pub fn complete_autoscan(
    window: tauri::WebviewWindow,
    controller: tauri::State<'_, Arc<AutoScanController>>,
    generation: u64,
    ticket_id: u64,
    succeeded: bool,
    owner: Option<CompareOwner>,
) -> Result<AutoScanStatusDto, String> {
    require_main(&window)?;
    controller.complete(generation, ticket_id, succeeded, owner.as_ref())
}
