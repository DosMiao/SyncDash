//! Authorization for renderer-requested settings changes.

pub(crate) mod grant;

use grant::SettingsAuthority;

pub(crate) fn authorize_log_directory_change(
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
            authorize_log_directory_change(&custom, &custom, "revision", None, &authority).is_ok()
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
            authorize_log_directory_change(&previous, &next, "revision", None, &authority)
                .unwrap_err()
                .contains("native picker")
        );

        let grant = authority
            .issue_log_directory_grant("revision", next.wanted_log_dir())
            .unwrap();
        assert!(authorize_log_directory_change(
            &previous,
            &next,
            "revision",
            Some(&grant),
            &authority
        )
        .is_ok());
        assert!(authorize_log_directory_change(
            &previous,
            &next,
            "revision",
            Some(&grant),
            &authority
        )
        .is_err());
    }
}
