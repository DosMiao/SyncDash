use std::sync::Arc;

use crate::contracts::settings::SettingsSaveDto;
use crate::features::operations::lifecycle::RunLifecycle;
use crate::features::settings::authorization::grant::SettingsAuthority;
use crate::ipc::{require_window_role, WindowRole};

#[tauri::command]
pub fn save_settings(
    window: tauri::WebviewWindow,
    settings: syncdash::store::settings::AppSettings,
    expected_revision: String,
    log_directory_grant: Option<String>,
    settings_authority: tauri::State<'_, Arc<SettingsAuthority>>,
    run_lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    app_log: tauri::State<'_, Arc<syncdash::obs::logging::AppLogSink>>,
) -> Result<SettingsSaveDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    run_lifecycle.with_idle_mutation("Changing log settings", || {
        let saved = crate::features::settings::save::save(
            &settings,
            &expected_revision,
            log_directory_grant.as_deref(),
            &settings_authority,
            &app_log,
        )?;
        Ok(SettingsSaveDto {
            snapshot: saved.snapshot.into(),
            migration: saved.migration,
        })
    })
}
