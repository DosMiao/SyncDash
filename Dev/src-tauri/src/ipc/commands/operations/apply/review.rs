//! Apply-review command delivery: authorize the caller and delegate to feature use cases.

use std::sync::Arc;

use crate::contracts::compare::{CompareIdentity, ReviewedRowDecisionDto};
use crate::contracts::operations::{AuthorizationDto, OperationReviewDto};
use crate::features::autoscan::controller::AutoScanController;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub async fn review_apply(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    compare_identity: CompareIdentity,
    reviewed_row_decisions: Vec<ReviewedRowDecisionDto>,
) -> Result<OperationReviewDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    crate::features::operations::apply::review::review_apply(
        lifecycle.inner().clone(),
        results.inner().clone(),
        authorizations.inner().clone(),
        compare_identity,
        reviewed_row_decisions,
    )
    .await
}

#[tauri::command]
pub async fn authorize_autoscan_apply(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    generation: u64,
    ticket_id: u64,
) -> Result<AuthorizationDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    crate::features::operations::apply::review::authorize_autoscan_apply(
        lifecycle.inner().clone(),
        results.inner().clone(),
        authorizations.inner().clone(),
        autoscan.inner().clone(),
        generation,
        ticket_id,
    )
    .await
}
