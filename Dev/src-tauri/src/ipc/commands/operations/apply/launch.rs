//! Apply command delivery: authorize the caller and delegate to the feature use case.

use std::sync::Arc;

use crate::contracts::operations::ApplyDto;
use crate::features::autoscan::controller::AutoScanController;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::events::repository::RunEventRepository;
use crate::features::operations::lifecycle::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

#[allow(clippy::too_many_arguments)] // Tauri injects state and exposes the rest as flat IPC fields.
#[tauri::command]
pub async fn apply_job(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    events: tauri::State<'_, Arc<RunEventRepository>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    authorization_token: String,
    launch_id: Option<u64>,
) -> Result<ApplyDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    crate::features::operations::apply::execution::apply_job(
        app,
        lifecycle.inner().clone(),
        results.inner().clone(),
        events.inner().clone(),
        authorizations.inner().clone(),
        autoscan.inner().clone(),
        authorization_token,
        launch_id,
    )
    .await
}
