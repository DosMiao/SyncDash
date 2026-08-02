use std::sync::Arc;

use tauri_plugin_dialog::DialogExt;

use crate::dto::{LogDirectorySelectionDto, SettingsSaveDto, SettingsSnapshotDto};
use crate::run_lifecycle::RunLifecycle;
use crate::settings_authority::SettingsAuthority;
use crate::window_role::{require_window_role, WindowRole};

const RUN_HISTORY_LIMIT: usize = 100;
const LOG_ARTIFACT_LINE_LIMIT: usize = 5_000;

fn authorize_log_directory_change(
    previous: &syncdash::store::settings::AppSettings,
    next: &syncdash::store::settings::AppSettings,
    expected_revision: &str,
    grant_id: Option<&str>,
    authority: &SettingsAuthority,
) -> Result<(), String> {
    let previous_directory = previous.wanted_log_dir();
    let next_directory = next.wanted_log_dir();
    if previous_directory == next_directory
        || next_directory == syncdash::store::settings::default_log_dir()
    {
        return Ok(());
    }
    let grant_id = grant_id.ok_or_else(|| {
        "Changing the log directory requires a fresh selection from the native picker".to_string()
    })?;
    authority.consume_log_directory_grant(grant_id, expected_revision, &next_directory)
}

#[tauri::command]
pub fn latest_run_records(
    window: tauri::WebviewWindow,
) -> Result<Vec<syncdash::obs::runlog::LatestRunRecord>, String> {
    require_window_role(&window, WindowRole::Main)?;
    syncdash::obs::runlog::latest_by_job().map_err(|error| error.to_string())
}

/// Include interrupted runs that have a directory but no final index entry; they are the runs whose
/// evidence is most important to retain.
#[tauri::command]
pub fn log_runs(
    window: tauri::WebviewWindow,
    job_id: Option<String>,
) -> Result<Vec<syncdash::obs::runlog::RunRecord>, String> {
    require_window_role(&window, WindowRole::Main)?;
    syncdash::obs::runlog::history_merged_for_registered_job(job_id.as_deref(), RUN_HISTORY_LIMIT)
        .map_err(|error| error.to_string())
}

/// Apply manifests can contain tens of thousands of lines, so the server fixes the IPC memory bound.
#[tauri::command]
pub fn log_artifact(
    window: tauri::WebviewWindow,
    record_id: String,
    artifact: syncdash::obs::runlog::LogArtifactKind,
) -> Result<Vec<String>, String> {
    require_window_role(&window, WindowRole::Main)?;
    syncdash::obs::runlog::artifact_lines(&record_id, artifact, LOG_ARTIFACT_LINE_LIMIT)
}

#[tauri::command]
pub fn reveal_log_location(
    window: tauri::WebviewWindow,
    record_id: Option<String>,
) -> Result<(), String> {
    require_window_role(&window, WindowRole::Main)?;
    syncdash::obs::runlog::with_validated_reveal_target(
        record_id.as_deref(),
        crate::cmd::shell::reveal_path,
    )
}

#[tauri::command]
pub fn get_settings(window: tauri::WebviewWindow) -> Result<SettingsSnapshotDto, String> {
    require_window_role(&window, WindowRole::Main)?;
    Ok(syncdash::store::settings::load_snapshot().into())
}

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
        settings.validate()?;
        let previous = syncdash::store::settings::load_snapshot();
        if previous.revision != expected_revision {
            return Err(format!(
                "Settings changed on disk (expected revision {expected_revision}, found {}) — reload before saving",
                previous.revision
            ));
        }
        let old_dir = app_log.directory();
        let new_dir = settings.wanted_log_dir();
        authorize_log_directory_change(
            &previous.settings,
            &settings,
            &expected_revision,
            log_directory_grant.as_deref(),
            &settings_authority,
        )?;
        syncdash::obs::logging::AppLogSink::validate_target(&new_dir, settings.level).map_err(
            |error| {
                format!("The new log directory is unusable; settings were not changed: {error}")
            },
        )?;
        let update = syncdash::store::settings::save_if_revision(&settings, &expected_revision)
            .map_err(|error| error.to_string())?;
        let saved_snapshot = update.snapshot.clone();
        let switched = app_log.reconfigure_after(&new_dir, settings.level, || {
            if old_dir != new_dir {
                syncdash::store::migrate::migrate_log_dir(&old_dir, &new_dir)
            } else {
                syncdash::store::migrate::MigrateReport::default()
            }
        });
        let report = match switched {
            Ok(report) => report,
            Err(error) => {
                return Err(match update.rollback() {
                    Ok(_) => format!(
                        "The new log directory became unusable; settings were restored, but migrated history may remain in the selected directory: {error}"
                    ),
                    Err(rollback_error) => format!(
                        "The new log directory became unusable ({error}) and restoring the previous settings failed: {rollback_error}"
                    ),
                });
            }
        };
        let dropped = syncdash::obs::runlog::prune(settings.keep_days, settings.max_total_mb)
            .map_err(|error| format!("Log cleanup failed: {error}"))?;
        if dropped > 0 {
            syncdash::log_info!(
                "settings",
                "Log cleanup: removed the records of {dropped} runs"
            );
        }
        Ok(SettingsSaveDto {
            snapshot: saved_snapshot.into(),
            migration: report,
        })
    })
}

#[tauri::command]
pub async fn pick_log_directory(
    window: tauri::WebviewWindow,
    expected_revision: String,
    settings_authority: tauri::State<'_, Arc<SettingsAuthority>>,
) -> Result<Option<LogDirectorySelectionDto>, String> {
    require_window_role(&window, WindowRole::Main)?;
    let snapshot = syncdash::store::settings::load_snapshot();
    if snapshot.revision != expected_revision {
        return Err(format!(
            "Settings changed on disk (expected revision {expected_revision}, found {}) — reload before choosing a log directory",
            snapshot.revision
        ));
    }

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
    let latest = syncdash::store::settings::load_snapshot();
    if latest.revision != expected_revision {
        return Err(format!(
            "Settings changed while the directory picker was open (expected revision {expected_revision}, found {}) — reload before choosing it again",
            latest.revision
        ));
    }
    let is_default = directory == syncdash::store::settings::default_log_dir();
    let directory_text = if is_default {
        String::new()
    } else {
        directory
            .to_str()
            .ok_or_else(|| {
                "The selected log directory cannot be represented in the settings file".to_string()
            })?
            .to_string()
    };
    let grant_id = if is_default || directory == latest.settings.wanted_log_dir() {
        None
    } else {
        Some(settings_authority.issue_log_directory_grant(&expected_revision, directory)?)
    };
    Ok(Some(LogDirectorySelectionDto {
        directory: directory_text,
        grant_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_at(directory: &str) -> syncdash::store::settings::AppSettings {
        syncdash::store::settings::AppSettings {
            log_dir: directory.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn unchanged_and_default_log_locations_need_no_renderer_grant() {
        let authority = SettingsAuthority::default();
        let custom = settings_at("/selected/logs");
        assert!(
            authorize_log_directory_change(&custom, &custom, "revision", None, &authority,).is_ok()
        );
        assert!(authorize_log_directory_change(
            &custom,
            &syncdash::store::settings::AppSettings::default(),
            "revision",
            None,
            &authority,
        )
        .is_ok());
    }

    #[test]
    fn a_changed_custom_log_location_consumes_an_exact_picker_grant() {
        let authority = SettingsAuthority::default();
        let previous = settings_at("/old/logs");
        let next = settings_at("/selected/logs");
        assert!(
            authorize_log_directory_change(&previous, &next, "revision", None, &authority,)
                .unwrap_err()
                .contains("native picker")
        );

        let directory = next.wanted_log_dir();
        let grant = authority
            .issue_log_directory_grant("revision", directory)
            .unwrap();
        assert!(authorize_log_directory_change(
            &previous,
            &next,
            "revision",
            Some(&grant),
            &authority,
        )
        .is_ok());
        assert!(authorize_log_directory_change(
            &previous,
            &next,
            "revision",
            Some(&grant),
            &authority,
        )
        .is_err());
    }
}
