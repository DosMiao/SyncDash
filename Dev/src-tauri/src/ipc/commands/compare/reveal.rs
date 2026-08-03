use std::sync::Arc;

use crate::contracts::compare::{CompareFileSideDto, CompareIdentity};
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::compare::export::receipt::CsvExportReceiptRepository;
use crate::features::compare::reveal::compare_row_path;
use crate::ipc::{require_window_role, WindowRole};

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
    let path = compare_row_path(&results, &compare_identity, index, side, direction_reversed)?;
    crate::ipc::native::reveal::reveal_path(&path)
}

#[tauri::command]
pub fn reveal_csv_export(
    window: tauri::WebviewWindow,
    receipts: tauri::State<'_, Arc<CsvExportReceiptRepository>>,
    receipt_id: String,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Main)?;
    receipts.consume_with(&receipt_id, crate::ipc::native::reveal::reveal_path)
}
