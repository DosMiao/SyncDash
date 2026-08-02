//! Settings and log-directory wire contracts.

use serde::Serialize;

#[derive(Serialize, Clone, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct SettingsNumericLimitsDto {
    #[ts(type = "number")]
    pub(crate) maximum_keep_days: u64,
    #[ts(type = "number")]
    pub(crate) maximum_total_mb: u64,
}

#[derive(Serialize, Clone, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct SettingsSnapshotDto {
    pub(crate) settings: syncdash::store::settings::AppSettings,
    pub(crate) revision: String,
    pub(crate) diagnostic: Option<String>,
    pub(crate) numeric_limits: SettingsNumericLimitsDto,
}

impl From<syncdash::store::settings::AppSettingsSnapshot> for SettingsSnapshotDto {
    fn from(snapshot: syncdash::store::settings::AppSettingsSnapshot) -> Self {
        Self {
            settings: snapshot.settings,
            revision: snapshot.revision,
            diagnostic: snapshot.diagnostic,
            numeric_limits: SettingsNumericLimitsDto {
                maximum_keep_days: syncdash::store::settings::MAX_KEEP_DAYS,
                maximum_total_mb: syncdash::store::settings::MAX_TOTAL_MB,
            },
        }
    }
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct LogDirectorySelectionDto {
    pub(crate) directory: String,
    pub(crate) grant_id: Option<String>,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct SettingsSaveDto {
    pub(crate) snapshot: SettingsSnapshotDto,
    pub(crate) migration: syncdash::run::history::MigrateReport,
}
