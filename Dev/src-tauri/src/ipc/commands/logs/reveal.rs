use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub fn reveal_log_location(
    window: tauri::WebviewWindow,
    record_id: Option<String>,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Main)?;
    syncdash::run::history::with_validated_reveal_target(
        record_id.as_deref(),
        crate::ipc::native::reveal::reveal_path,
    )
}
