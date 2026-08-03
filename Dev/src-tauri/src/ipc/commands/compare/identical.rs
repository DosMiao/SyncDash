use std::sync::Arc;

use crate::contracts::compare::{CompareIdentity, IdenticalPage};
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::lifecycle::RunLifecycle;
use crate::ipc::{require_window_role, WindowRole};

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
    results.identical_page(&compare_identity, &query, offset, limit)
}
