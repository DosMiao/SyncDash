use std::sync::Arc;

use crate::contracts::operations::PostRunPowerActionDto;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub fn execute_post_run_power_action(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    run_id: u64,
    action: PostRunPowerActionDto,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Progress)?;
    lifecycle.consume_post_run_power_action_grant_with(run_id, || {
        crate::ipc::native::power::launch(action)
    })
}
