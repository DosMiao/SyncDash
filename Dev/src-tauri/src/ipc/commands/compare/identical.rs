use std::sync::Arc;

use syncdash::pipeline::compare;

use crate::contracts::compare::{CompareIdentity, IdenticalPage};
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;
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
    let retained = results
        .get_exact(&compare_identity)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "This exact Compare result is no longer retained — run Compare again".to_string()
        })?;
    let (total, rows) = compare::evidence::identical_page(
        retained.source(),
        retained.target(),
        retained.compare_options(),
        &query,
        offset,
        limit.min(2000),
    );
    Ok(IdenticalPage { total, rows })
}
