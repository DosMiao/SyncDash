//! Revision checks and path normalization around native directory selection.

use std::path::PathBuf;

use super::authorization::grant::SettingsAuthority;

pub(crate) enum SelectionRevisionPhase {
    BeforePicker,
    AfterPicker,
}

pub(crate) fn require_revision(
    expected_revision: &str,
    phase: SelectionRevisionPhase,
) -> Result<syncdash::store::settings::AppSettingsSnapshot, String> {
    let snapshot = syncdash::store::settings::load_snapshot();
    if snapshot.revision != expected_revision {
        return Err(match phase {
            SelectionRevisionPhase::BeforePicker => format!(
                "Settings changed on disk (expected revision {expected_revision}, found {}) — reload before choosing a log directory",
                snapshot.revision
            ),
            SelectionRevisionPhase::AfterPicker => format!(
                "Settings changed while the directory picker was open (expected revision {expected_revision}, found {}) — reload before choosing it again",
                snapshot.revision
            ),
        });
    }
    Ok(snapshot)
}

pub(crate) struct LogDirectorySelection {
    pub(crate) directory: String,
    pub(crate) grant_id: Option<String>,
}

pub(crate) fn authorize_selection(
    directory: PathBuf,
    snapshot: &syncdash::store::settings::AppSettingsSnapshot,
    expected_revision: &str,
    authority: &SettingsAuthority,
) -> Result<LogDirectorySelection, String> {
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
    let grant_id = if is_default || directory == snapshot.settings.wanted_log_dir() {
        None
    } else {
        Some(authority.issue_log_directory_grant(expected_revision, directory)?)
    };
    Ok(LogDirectorySelection {
        directory: directory_text,
        grant_id,
    })
}
