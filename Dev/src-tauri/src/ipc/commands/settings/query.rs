use crate::contracts::settings::SettingsSnapshotDto;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub fn get_settings(window: tauri::WebviewWindow) -> Result<SettingsSnapshotDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(syncdash::store::settings::load_snapshot().into())
}
