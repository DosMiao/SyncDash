//! Transactional settings persistence, log migration, and sink reconfiguration.

use super::authorization::authorize_log_directory_change;
use super::authorization::grant::SettingsAuthority;

pub(crate) struct SavedSettings {
    pub(crate) snapshot: syncdash::store::settings::AppSettingsSnapshot,
    pub(crate) migration: syncdash::run::history::MigrateReport,
}

pub(crate) fn save(
    settings: &syncdash::store::settings::AppSettings,
    expected_revision: &str,
    log_directory_grant: Option<&str>,
    authority: &SettingsAuthority,
    app_log: &syncdash::obs::logging::AppLogSink,
) -> Result<SavedSettings, String> {
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
        settings,
        expected_revision,
        log_directory_grant,
        authority,
    )?;
    syncdash::obs::logging::AppLogSink::validate_target(&new_dir, settings.level).map_err(
        |error| format!("The new log directory is unusable; settings were not changed: {error}"),
    )?;
    let update = syncdash::store::settings::save_if_revision(settings, expected_revision)
        .map_err(|error| error.to_string())?;
    let saved_snapshot = update.snapshot.clone();
    let switched = app_log.reconfigure_after(&new_dir, settings.level, || {
        if old_dir != new_dir {
            syncdash::run::history::migrate_log_dir(&old_dir, &new_dir)
        } else {
            syncdash::run::history::MigrateReport::default()
        }
    });
    let migration = match switched {
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
    let dropped = syncdash::run::history::prune(settings.keep_days, settings.max_total_mb)
        .map_err(|error| format!("Log cleanup failed: {error}"))?;
    if dropped > 0 {
        syncdash::log_info!(
            "settings",
            "Log cleanup: removed the records of {dropped} runs"
        );
    }
    Ok(SavedSettings {
        snapshot: saved_snapshot,
        migration,
    })
}
