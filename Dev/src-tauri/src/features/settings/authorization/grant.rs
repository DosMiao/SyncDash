//! Expiring one-use grants bound to an exact settings revision and directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const GRANT_LIFETIME: Duration = Duration::from_secs(15 * 60);

struct LogDirectoryGrant {
    settings_revision: String,
    directory: PathBuf,
    issued_at: Instant,
}

#[derive(Default)]
pub(crate) struct SettingsAuthority {
    grants: Mutex<HashMap<String, LogDirectoryGrant>>,
}

impl SettingsAuthority {
    pub(crate) fn issue_log_directory_grant(
        &self,
        settings_revision: &str,
        directory: PathBuf,
    ) -> Result<String, String> {
        let grant_id =
            crate::secure_random::random_hex::<16>("Cannot create a directory-selection grant")?;
        let now = Instant::now();
        let mut grants = self
            .grants
            .lock()
            .map_err(|_| "The log-directory grant store is unavailable".to_string())?;
        grants.retain(|_, grant| now.duration_since(grant.issued_at) <= GRANT_LIFETIME);
        grants.insert(
            grant_id.clone(),
            LogDirectoryGrant {
                settings_revision: settings_revision.to_string(),
                directory,
                issued_at: now,
            },
        );
        Ok(grant_id)
    }

    pub(crate) fn consume_log_directory_grant(
        &self,
        grant_id: &str,
        expected_settings_revision: &str,
        directory: &Path,
    ) -> Result<(), String> {
        let now = Instant::now();
        let mut grants = self
            .grants
            .lock()
            .map_err(|_| "The log-directory grant store is unavailable".to_string())?;
        grants.retain(|_, grant| now.duration_since(grant.issued_at) <= GRANT_LIFETIME);
        let grant = grants.remove(grant_id).ok_or_else(|| {
            "The log-directory selection expired — choose the directory again".to_string()
        })?;
        if grant.settings_revision != expected_settings_revision {
            return Err("Settings changed after the log directory was selected — reload settings and choose it again".into());
        }
        if grant.directory != directory {
            return Err("The requested log directory does not match the directory selected in the native picker".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_is_bound_to_revision_and_directory_and_consumed_once() {
        let authority = SettingsAuthority::default();
        let directory = PathBuf::from("/tmp/syncdash-logs");
        let grant = authority
            .issue_log_directory_grant("revision-a", directory.clone())
            .unwrap();
        assert!(authority
            .consume_log_directory_grant(&grant, "revision-a", &directory)
            .is_ok());
        assert!(authority
            .consume_log_directory_grant(&grant, "revision-a", &directory)
            .unwrap_err()
            .contains("expired"));
    }

    #[test]
    fn a_mismatch_invalidates_the_grant() {
        let authority = SettingsAuthority::default();
        let directory = PathBuf::from("/tmp/syncdash-logs");
        let grant = authority
            .issue_log_directory_grant("revision-a", directory.clone())
            .unwrap();
        assert!(authority
            .consume_log_directory_grant(&grant, "revision-b", &directory)
            .unwrap_err()
            .contains("Settings changed"));
        assert!(authority
            .consume_log_directory_grant(&grant, "revision-a", &directory)
            .is_err());
    }
}
