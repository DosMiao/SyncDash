use std::sync::Arc;

use tauri_plugin_dialog::DialogExt;

use crate::contracts::settings::LogDirectorySelectionDto;
use crate::features::settings::authorization::grant::SettingsAuthority;
use crate::features::settings::selection::{
    authorize_selection, require_revision, SelectionRevisionPhase,
};
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub async fn pick_log_directory(
    window: tauri::WebviewWindow,
    expected_revision: String,
    settings_authority: tauri::State<'_, Arc<SettingsAuthority>>,
) -> Result<Option<LogDirectorySelectionDto>, String> {
    require_window_role(&window, WindowRole::Main)?;
    let snapshot = require_revision(&expected_revision, SelectionRevisionPhase::BeforePicker)?;
    let (sender, mut receiver) = tauri::async_runtime::channel(1);
    window
        .dialog()
        .file()
        .set_title("Select a log directory")
        .set_directory(snapshot.settings.wanted_log_dir())
        .pick_folder(move |selection| {
            let _ = sender.try_send(selection);
        });
    let selected = receiver.recv().await.ok_or_else(|| {
        "The native directory picker closed without reporting a result".to_string()
    })?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let directory = selected
        .into_path()
        .map_err(|error| format!("The selected log location is not a filesystem path: {error}"))?;
    let latest = require_revision(&expected_revision, SelectionRevisionPhase::AfterPicker)?;
    let selection =
        authorize_selection(directory, &latest, &expected_revision, &settings_authority)?;
    Ok(Some(LogDirectorySelectionDto {
        directory: selection.directory,
        grant_id: selection.grant_id,
    }))
}
