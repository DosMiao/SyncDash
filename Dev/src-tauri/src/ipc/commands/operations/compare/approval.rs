//! Approval command delivery: authorize the caller and delegate to the feature use case.

use std::sync::Arc;

use crate::contracts::operations::{AuthorizationDto, OperationApprovalDto};
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::lifecycle::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub fn approve_operation(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    challenge_id: String,
    approval: OperationApprovalDto,
) -> Result<AuthorizationDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    crate::features::operations::compare::approval::approve_operation(
        lifecycle.inner(),
        authorizations.inner(),
        challenge_id,
        approval,
    )
}
