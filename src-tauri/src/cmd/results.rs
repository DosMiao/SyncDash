//! Reading a finished compare: the identical-items page, and CSV export.

use std::sync::Arc;

use syncdash::job;
use syncdash::pipeline::compare;
use tauri_plugin_dialog::DialogExt;

use crate::compare_results::{
    CompareResultForgetOutcome, CompareResultRepository, CompareWorkspaceJobState,
};
use crate::csv_export::{
    default_export_filename, operation_side_paths, presented_operation, write_compare_csv,
};
use crate::csv_export_receipts::CsvExportReceiptRepository;
use crate::dto::{
    CompareFileSideDto, CompareIdentity, CompareResultForgetDto, CompareWorkspaceLookupDto,
    CsvExportDto, CsvRowPresentationDto, IdenticalPage,
};
use crate::job_target::resolve_target;
use crate::run_lifecycle::RunLifecycle;
use crate::window_role::{require_window_role, WindowRole};

/// Reconcile and restore one exact durably retained workspace as one repository operation. Its plan
/// remains available for inspection after a job mutation, but the returned execution status is then
/// expired and cannot authorize Apply.
#[tauri::command]
pub fn reconcile_compare_workspace(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    compare_identity: CompareIdentity,
) -> Result<CompareWorkspaceLookupDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    let _command = lifecycle.inner().command_lease()?;
    let job_state = match job::load_by_id(&compare_identity.job_id) {
        Ok((job_name, full_job)) => {
            let config_revision = job::config_revision(&full_job)
                .map_err(|error| format!("Job '{job_name}': {error}"))?;
            if config_revision == compare_identity.config_revision {
                resolve_target(&full_job, Some(compare_identity.target_index))?;
                CompareWorkspaceJobState::Current { job_name }
            } else {
                CompareWorkspaceJobState::ConfigurationChanged
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            CompareWorkspaceJobState::Deleted
        }
        Err(error) => return Err(error.to_string()),
    };
    results
        .reconcile_exact_workspace(&compare_identity, job_state)
        .map_err(|error| error.to_string())
}

/// Restore the most recent successful result for one caller-observed job/target/revision scope.
/// The expected revision prevents a delayed selection request from crossing into a newer job
/// configuration, while the command lease excludes a concurrent registry mutation after the read.
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
    require_expected_config_revision(
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
    let outcome = results
        .forget(&compare_identity)
        .map_err(|error| error.to_string())?;
    Ok(match outcome {
        CompareResultForgetOutcome::Forgotten { cleanup_warning } => {
            CompareResultForgetDto::Forgotten { cleanup_warning }
        }
        CompareResultForgetOutcome::AlreadyForgotten => CompareResultForgetDto::AlreadyForgotten,
    })
}

fn require_expected_config_revision(
    job_name: &str,
    expected_config_revision: &str,
    current_config_revision: &str,
) -> Result<(), String> {
    if current_config_revision != expected_config_revision {
        return Err(format!(
            "Job '{job_name}' changed before its Compare workspace could be restored — refresh the job and try again"
        ));
    }
    Ok(())
}

/// Pagination for the "Identical" panel from that result's retained snapshots — no rescan.
#[tauri::command]
pub fn list_identical(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    compare_identity: CompareIdentity,
    query: String,
    offset: usize,
    limit: usize,
) -> Result<IdenticalPage, String> {
    require_window_role(&window, WindowRole::Main)?;
    let _command = lifecycle.inner().command_lease()?;
    let retained = results
        .get_exact(&compare_identity)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "This exact Compare result is no longer retained — run Compare again".to_string()
        })?;
    let (total, rows) = compare::evidence::identical_page(
        retained.source(),
        retained.target(),
        retained.compare_options(),
        &query,
        offset,
        limit.min(2000),
    );
    Ok(IdenticalPage { total, rows })
}

#[tauri::command]
pub async fn export_compare_csv(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    receipts: tauri::State<'_, Arc<CsvExportReceiptRepository>>,
    compare_identity: CompareIdentity,
    rows: Vec<CsvRowPresentationDto>,
) -> Result<CsvExportDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    let retained = results
        .get_exact(&compare_identity)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "This exact Compare result is no longer retained — run Compare again".to_string()
        })?;
    let default_filename = default_export_filename(
        &retained.owner().job_name,
        retained.identity().compare_run_id,
        syncdash::foundation::time::now_ms(),
    )?;
    let header = retained.plan_header().clone();
    let operations = retained.plan_operations().to_vec();
    let metadata = retained.plan_metadata().to_vec();
    drop(retained);

    let selected = app
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Export Compare result")
        .set_file_name(default_filename)
        .add_filter("CSV document", &["csv"])
        .blocking_save_file();
    let Some(selected) = selected else {
        return Ok(CsvExportDto::Cancelled);
    };
    let destination = selected
        .into_path()
        .map_err(|error| format!("The selected export destination is invalid: {error}"))?;
    let receipt_id = receipts.issue(destination.clone())?;
    let write_result = tauri::async_runtime::spawn_blocking({
        let destination = destination.clone();
        move || {
            let mut staged = syncdash::fs::staged::Staged::create(&destination)
                .map_err(|error| format!("{}: {error}", destination.display()))?;
            let row_count = write_compare_csv(&mut staged, &header, &operations, &metadata, &rows)?;
            staged
                .seal(true)
                .and_then(|()| staged.commit())
                .map_err(|error| format!("{}: {error}", destination.display()))?;
            Ok::<usize, String>(row_count)
        }
    })
    .await;
    let row_count = match write_result {
        Ok(Ok(row_count)) => row_count,
        Ok(Err(error)) => {
            receipts.revoke(&receipt_id);
            return Err(error);
        }
        Err(error) => {
            receipts.revoke(&receipt_id);
            return Err(format!("The CSV export worker failed: {error}"));
        }
    };
    Ok(CsvExportDto::Exported {
        row_count,
        display_path: destination.to_string_lossy().into_owned(),
        receipt_id,
    })
}

fn local_compare_path(root: &str, relative: &str) -> Result<std::path::PathBuf, String> {
    let relative = syncdash::foundation::path::RootRelativePath::new(relative)
        .map_err(|error| error.to_string())?;
    let syncdash::fs::vfs::spec::RootSpec::Local(root) = syncdash::fs::vfs::spec::parse(root)
    else {
        return Err("File Manager reveal is only available for roots on this computer".into());
    };
    Ok(syncdash::foundation::path::join_native(
        &root,
        relative.as_str(),
    ))
}

#[tauri::command]
pub fn reveal_compare_row(
    window: tauri::WebviewWindow,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    compare_identity: CompareIdentity,
    index: usize,
    side: CompareFileSideDto,
    direction_reversed: bool,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Main)?;
    let retained = results
        .get_exact(&compare_identity)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "This exact Compare result is no longer retained — run Compare again".to_string()
        })?;
    let operation = presented_operation(retained.plan_operations(), index, direction_reversed)?;
    let (source_relative, target_relative) = operation_side_paths(&operation);
    let (root, relative, side_name) = match side {
        CompareFileSideDto::Source => (
            retained.plan_header().source_root.as_str(),
            source_relative,
            "source",
        ),
        CompareFileSideDto::Target => (
            retained.plan_header().target_root.as_str(),
            target_relative,
            "target",
        ),
    };
    let relative = relative
        .ok_or_else(|| format!("Compare row {} has no {side_name}-side path", index + 1))?;
    let path = local_compare_path(root, relative)?;
    crate::cmd::shell::reveal_path(&path)
}

#[tauri::command]
pub fn reveal_csv_export(
    window: tauri::WebviewWindow,
    receipts: tauri::State<'_, Arc<CsvExportReceiptRepository>>,
    receipt_id: String,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Main)?;
    receipts.consume_with(&receipt_id, crate::cmd::shell::reveal_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_revision_rejects_a_delayed_selection_request() {
        assert!(require_expected_config_revision("Archive", "revision-a", "revision-a").is_ok());
        let error =
            require_expected_config_revision("Archive", "revision-a", "revision-b").unwrap_err();
        assert!(error.contains("changed before its Compare workspace could be restored"));
    }

    #[test]
    fn reveal_paths_accept_only_safe_entries_under_local_roots() {
        let path = local_compare_path("/root", "folder/file.txt").unwrap();
        assert_eq!(
            path,
            syncdash::foundation::path::join_native(
                std::path::Path::new("/root"),
                "folder/file.txt",
            )
        );
        assert!(local_compare_path("/root", "../outside").is_err());
        assert!(local_compare_path("sftp://host/root", "file.txt").is_err());
    }
}
