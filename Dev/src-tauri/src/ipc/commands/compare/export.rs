use std::sync::Arc;

use tauri_plugin_dialog::DialogExt;

use crate::contracts::compare::{CompareIdentity, CsvExportDto, CsvRowPresentationDto};
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::compare::export::filename::default_export_filename;
use crate::features::compare::export::receipt::CsvExportReceiptRepository;
use crate::features::compare::export::render::write_compare_csv;
use crate::ipc::{require_window_role, WindowRole};

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
