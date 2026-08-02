use std::sync::Arc;

use crate::contracts::compare::{CompareFileSideDto, CompareIdentity};
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::compare::export::presentation::{operation_side_paths, presented_operation};
use crate::features::compare::export::receipt::CsvExportReceiptRepository;
use crate::features::compare::reveal::local_compare_path;
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
    crate::ipc::native::reveal::reveal_path(&local_compare_path(root, relative)?)
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
