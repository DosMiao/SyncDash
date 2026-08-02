//! v0.10: app-level settings.
//!
//! Until now the project had only per-job TOML (`config.rs`) and frontend localStorage — "a
//! configurable log directory" needs an app-level home, and this is the first one. Location:
//! `<config>/settings.toml`, alongside the jobs directory.
//!
//! Same rule as `runlog`: **failing to read settings must never block a sync**. A parse failure or an
//! unwritable directory always falls back to a usable value and leaves a log line — never propagates up.

use crate::model::event::LogLevel;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

pub const MAX_KEEP_DAYS: u64 = 36_500;
pub const MAX_TOTAL_MB: u64 = 1_048_576;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct AppSettings {
    /// Empty = default `<config>/logs`
    #[serde(default)]
    pub log_dir: String,
    /// `Log` events below this level are not written to disk
    #[serde(default = "default_level")]
    pub level: LogLevel,
    /// Run records older than this many days get pruned; 0 = no day-based pruning
    #[serde(default = "default_keep_days")]
    #[ts(type = "number")]
    pub keep_days: u64,
    /// Total log size cap (MB); over it, the oldest go first. 0 = unlimited
    #[serde(default = "default_max_total_mb")]
    #[ts(type = "number")]
    pub max_total_mb: u64,
    /// Logging granularity for compare runs: `summary` (index line only, no directory) | `off`.
    /// No `full` tier: watch on a 30s cycle = 2880 runs a day, and creating a directory each time would swamp the log disk.
    #[serde(default = "default_log_compare")]
    pub log_compare: String,
    /// CLI: also mirror log lines verbatim to stderr (keeps the pre-refactor terminal experience)
    #[serde(default = "default_true")]
    pub mirror_stderr: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            log_dir: String::new(),
            level: default_level(),
            keep_days: default_keep_days(),
            max_total_mb: default_max_total_mb(),
            log_compare: default_log_compare(),
            mirror_stderr: true,
        }
    }
}

fn default_level() -> LogLevel {
    LogLevel::Info
}

fn default_keep_days() -> u64 {
    30
}

fn default_max_total_mb() -> u64 {
    512
}

fn default_log_compare() -> String {
    "summary".into()
}

fn default_true() -> bool {
    true
}

/// Config root. Comes straight from `foundation::dirs`, not by walking up from the jobs dir.
pub fn config_dir() -> PathBuf {
    crate::foundation::dirs::config_dir()
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.toml")
}

pub fn default_log_dir() -> PathBuf {
    config_dir().join("logs")
}

impl AppSettings {
    /// The directory named in the config (may not exist, may not be writable). Migration needs it as the "old location", hence its own accessor.
    pub fn wanted_log_dir(&self) -> PathBuf {
        if self.log_dir.trim().is_empty() {
            default_log_dir()
        } else {
            PathBuf::from(self.log_dir.trim())
        }
    }

    /// The log directory that actually works: configured value → if it can't be created, the default → if that fails too, the temp dir.
    /// **Never returns a path that can't be written** — the logging system either writes, or quietly writes somewhere else.
    pub fn resolved_log_dir(&self) -> PathBuf {
        let want = self.wanted_log_dir();
        if std::fs::create_dir_all(&want).is_ok() {
            return want;
        }
        let fallback = default_log_dir();
        if want != fallback && std::fs::create_dir_all(&fallback).is_ok() {
            eprintln!(
                "settings: log dir {} unusable, falling back to {}",
                want.display(),
                fallback.display()
            );
            return fallback;
        }
        std::env::temp_dir().join("syncdash-logs")
    }

    pub fn logs_compare(&self) -> bool {
        self.log_compare != "off"
    }

    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.log_compare.as_str(), "summary" | "off") {
            return Err(format!(
                "log_compare must be 'summary' or 'off', got {:?}",
                self.log_compare
            ));
        }
        if self.log_dir.contains('\0') {
            return Err("log_dir contains a NUL byte".into());
        }
        if self.keep_days > MAX_KEEP_DAYS {
            return Err(format!("keep_days must be {MAX_KEEP_DAYS} or less"));
        }
        if self.max_total_mb > MAX_TOTAL_MB {
            return Err(format!("max_total_mb must be {MAX_TOTAL_MB} or less"));
        }
        self.max_total_mb
            .checked_mul(1024 * 1024)
            .ok_or_else(|| "max_total_mb is too large".to_string())?;
        self.keep_days
            .checked_mul(24 * 60 * 60 * 1000)
            .ok_or_else(|| "keep_days is too large".to_string())?;
        Ok(())
    }
}

/// Revision token for compare-and-swap settings updates. Valid files are identified by their
/// effective settings rather than TOML formatting; invalid files include their raw bytes so a
/// renderer that saw a fallback snapshot cannot overwrite a later repair.
pub fn revision(settings: &AppSettings, invalid_file: Option<&[u8]>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"syncdash-settings-v1\0");
    match invalid_file {
        Some(bytes) => {
            hasher.update(b"invalid\0");
            hasher.update(bytes);
        }
        None => {
            hasher.update(b"effective\0");
            let encoded = serde_json::to_vec(settings)
                .expect("AppSettings contains only infallibly serializable fields");
            hasher.update(&encoded);
        }
    }
    hasher.finalize().to_hex().to_string()
}

#[derive(Clone, Debug)]
pub struct AppSettingsSnapshot {
    pub settings: AppSettings,
    pub revision: String,
    pub diagnostic: Option<String>,
}

enum PreviousSettingsFile {
    Missing,
    Bytes(Vec<u8>),
}

pub struct AppSettingsUpdate {
    pub snapshot: AppSettingsSnapshot,
    path: PathBuf,
    previous_file: PreviousSettingsFile,
    changed: bool,
}

impl AppSettingsUpdate {
    pub fn rollback(self) -> std::io::Result<AppSettingsSnapshot> {
        if !self.changed {
            return Ok(self.snapshot);
        }
        let dir = self.path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "settings path has no parent directory",
            )
        })?;
        let lock = open_mutation_lock(dir)?;
        lock.lock()?;
        let current = load_snapshot_at(&self.path);
        if current.revision != self.snapshot.revision {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "settings changed again after saving revision {} — refusing to overwrite revision {} during rollback",
                    self.snapshot.revision, current.revision
                ),
            ));
        }
        match self.previous_file {
            PreviousSettingsFile::Missing => match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            },
            PreviousSettingsFile::Bytes(bytes) => write_settings_bytes(&self.path, &bytes)?,
        }
        Ok(load_snapshot_at(&self.path))
    }
}

fn load_snapshot_at(path: &std::path::Path) -> AppSettingsSnapshot {
    match std::fs::read(&path) {
        Ok(bytes) => match std::str::from_utf8(&bytes)
            .map_err(|error| error.to_string())
            .and_then(|text| toml::from_str::<AppSettings>(text).map_err(|error| error.to_string()))
            .and_then(|settings| settings.validate().map(|()| settings))
        {
            Ok(settings) => AppSettingsSnapshot {
                revision: revision(&settings, None),
                settings,
                diagnostic: None,
            },
            Err(error) => {
                let settings = AppSettings::default();
                AppSettingsSnapshot {
                    revision: revision(&settings, Some(&bytes)),
                    settings,
                    diagnostic: Some(format!(
                        "settings: {} is invalid, using defaults: {error}",
                        path.display()
                    )),
                }
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let settings = AppSettings::default();
            AppSettingsSnapshot {
                revision: revision(&settings, None),
                settings,
                diagnostic: None,
            }
        }
        Err(error) => {
            let settings = AppSettings::default();
            let diagnostic = format!(
                "settings: {} could not be read, using defaults: {error}",
                path.display()
            );
            AppSettingsSnapshot {
                revision: revision(&settings, Some(diagnostic.as_bytes())),
                settings,
                diagnostic: Some(diagnostic),
            }
        }
    }
}

pub fn load_snapshot() -> AppSettingsSnapshot {
    load_snapshot_at(&settings_path())
}

/// Read settings and preserve any fallback reason so startup can publish it after installing the
/// application log sink. A missing file is the normal first-run state and has no diagnostic.
pub fn load_with_diagnostic() -> (AppSettings, Option<String>) {
    let snapshot = load_snapshot();
    (snapshot.settings, snapshot.diagnostic)
}

pub fn load() -> AppSettings {
    load_with_diagnostic().0
}

fn write_settings_bytes(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut staged = crate::fs::staged::Staged::create(path)?;
    staged.write_all(bytes)?;
    staged.seal(true)?;
    staged.commit()
}

fn write_settings(path: &std::path::Path, settings: &AppSettings) -> std::io::Result<()> {
    let text = toml::to_string_pretty(settings).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("toml serialize: {error}"),
        )
    })?;
    write_settings_bytes(path, text.as_bytes())
}

fn open_mutation_lock(dir: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::create_dir_all(dir)?;
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(dir.join(".syncdash-settings.lock"))
}

fn save_if_revision_at(
    path: &std::path::Path,
    settings: &AppSettings,
    expected_revision: &str,
) -> std::io::Result<AppSettingsUpdate> {
    settings
        .validate()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "settings path has no parent directory",
        )
    })?;
    let lock = open_mutation_lock(dir)?;
    lock.lock()?;

    let current = load_snapshot_at(path);
    if current.revision != expected_revision {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "settings changed on disk (expected revision {expected_revision}, found {}) — reload before saving",
                current.revision
            ),
        ));
    }
    if current.diagnostic.is_none() && current.revision == revision(settings, None) {
        return Ok(AppSettingsUpdate {
            snapshot: current,
            path: path.to_path_buf(),
            previous_file: PreviousSettingsFile::Missing,
            changed: false,
        });
    }

    let previous_file = match std::fs::read(path) {
        Ok(bytes) => PreviousSettingsFile::Bytes(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PreviousSettingsFile::Missing,
        Err(error) => {
            return Err(std::io::Error::new(
                error.kind(),
                format!(
                    "cannot preserve {} for a safe settings rollback: {error}",
                    path.display()
                ),
            ))
        }
    };
    write_settings(path, settings)?;
    Ok(AppSettingsUpdate {
        snapshot: load_snapshot_at(path),
        path: path.to_path_buf(),
        previous_file,
        changed: true,
    })
}

pub fn save_if_revision(
    settings: &AppSettings,
    expected_revision: &str,
) -> std::io::Result<AppSettingsUpdate> {
    save_if_revision_at(&settings_path(), settings, expected_revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "syncdash-set-{tag}-{}",
            crate::foundation::time::now_ms()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        let s = AppSettings {
            log_dir: "D:\\logs".into(),
            level: LogLevel::Warn,
            keep_days: 7,
            ..Default::default()
        };
        let text = toml::to_string_pretty(&s).unwrap();
        let back: AppSettings = toml::from_str(&text).unwrap();
        assert_eq!(s, back);
        // Old config files with missing fields must still load — every field has a serde default
        let partial: AppSettings = toml::from_str("log_dir = ''").unwrap();
        assert_eq!(partial.level, LogLevel::Info);
        assert_eq!(partial.keep_days, 30);
        assert!(partial.mirror_stderr);
        assert!(partial.logs_compare());
    }

    #[test]
    fn invalid_settings_are_rejected_before_the_live_configuration_changes() {
        let mut settings = AppSettings::default();
        settings.log_compare = "everything".into();
        assert!(settings.validate().unwrap_err().contains("log_compare"));

        settings.log_compare = "summary".into();
        settings.log_dir = "bad\0path".into();
        assert!(settings.validate().unwrap_err().contains("NUL"));

        settings.log_dir.clear();
        settings.keep_days = MAX_KEEP_DAYS + 1;
        assert!(settings.validate().unwrap_err().contains("keep_days"));

        settings.keep_days = default_keep_days();
        settings.max_total_mb = MAX_TOTAL_MB + 1;
        assert!(settings.validate().unwrap_err().contains("max_total_mb"));
    }

    #[test]
    fn settings_revision_ignores_formatting_but_fences_invalid_file_changes() {
        let settings = AppSettings::default();
        assert_eq!(revision(&settings, None), revision(&settings.clone(), None));
        assert_ne!(
            revision(&settings, Some(b"not = [valid")),
            revision(&settings, Some(b"still = [invalid"))
        );
        assert_ne!(
            revision(&settings, None),
            revision(&settings, Some(b"not = [valid"))
        );
    }

    #[test]
    fn settings_save_refuses_a_stale_revision_and_preserves_the_newer_file() {
        let root = tmp("cas");
        let path = root.join("settings.toml");
        let initial = AppSettings::default();
        write_settings(&path, &initial).unwrap();
        let loaded = load_snapshot_at(&path);

        let mut newer = initial.clone();
        newer.keep_days = 14;
        let saved = save_if_revision_at(&path, &newer, &loaded.revision).unwrap();
        assert_eq!(saved.snapshot.settings.keep_days, 14);

        let mut stale = initial;
        stale.keep_days = 90;
        let error = match save_if_revision_at(&path, &stale, &loaded.revision) {
            Ok(_) => panic!("a stale settings revision must be refused"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(load_snapshot_at(&path).settings.keep_days, 14);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn saving_the_fallback_snapshot_repairs_the_same_invalid_revision() {
        let root = tmp("repair");
        let path = root.join("settings.toml");
        std::fs::write(&path, "not = [valid").unwrap();
        let invalid = load_snapshot_at(&path);
        assert!(invalid.diagnostic.is_some());

        let repaired = save_if_revision_at(&path, &invalid.settings, &invalid.revision).unwrap();
        assert!(repaired.snapshot.diagnostic.is_none());
        assert_eq!(repaired.snapshot.settings, AppSettings::default());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_restores_the_exact_invalid_file() {
        let root = tmp("rollback");
        let path = root.join("settings.toml");
        let invalid_bytes = b"not = [valid";
        std::fs::write(&path, invalid_bytes).unwrap();
        let invalid = load_snapshot_at(&path);

        let update = save_if_revision_at(&path, &invalid.settings, &invalid.revision).unwrap();
        let restored = update.rollback().unwrap();
        assert!(restored.diagnostic.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), invalid_bytes);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolved_log_dir_falls_back_when_unwritable() {
        // Put a **file** into the setting where a directory belongs: create_dir_all must fail → it has to fall back somewhere writable
        let root = tmp("fallback");
        let f = root.join("not-a-dir");
        std::fs::write(&f, "x").unwrap();
        let s = AppSettings {
            log_dir: f.display().to_string(),
            ..Default::default()
        };
        let got = s.resolved_log_dir();
        assert_ne!(got, f, "must not return a path that cannot be written");
        assert!(
            got.is_dir(),
            "the fallback target must actually exist: {}",
            got.display()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
