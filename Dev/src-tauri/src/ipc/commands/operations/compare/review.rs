//! Compare-review command delivery: authorize the caller and delegate to the feature use case.

use std::sync::Arc;

use crate::contracts::compare::AutoScanCompareRequestDto;
use crate::contracts::operations::OperationReviewDto;
use crate::features::autoscan::controller::AutoScanController;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub async fn review_compare(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    expected_job_id: String,
    target_index: Option<usize>,
    auto_scan_request: Option<AutoScanCompareRequestDto>,
) -> Result<OperationReviewDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    crate::features::operations::compare::review::review_compare(
        lifecycle.inner().clone(),
        authorizations.inner().clone(),
        autoscan.inner().clone(),
        expected_job_id,
        target_index,
        auto_scan_request,
    )
    .await
}
