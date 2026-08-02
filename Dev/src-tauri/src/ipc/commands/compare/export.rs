use std::sync::Arc;

use tauri_plugin_dialog::DialogExt;

use crate::contracts::compare::{CompareIdentity, CsvExportDto, CsvRowPresentationDto};
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::compare::export::execution;
use crate::features::compare::export::receipt::CsvExportReceiptRepository;
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
    let source = execution::prepare(&results, &compare_identity)?;

    // The native save dialog is delivery: it needs the window and the app handle, which is
    // exactly what a feature must not hold.
    let selected = app
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Export Compare result")
        .set_file_name(source.default_filename.clone())
        .add_filter("CSV document", &["csv"])
        .blocking_save_file();
    let Some(selected) = selected else {
        return Ok(CsvExportDto::Cancelled);
    };
    let destination = selected
        .into_path()
        .map_err(|error| format!("The selected export destination is invalid: {error}"))?;

    execution::write_to(Arc::clone(&receipts), source, destination, rows).await
}
