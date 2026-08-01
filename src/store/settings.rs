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
        self.max_total_mb
            .checked_mul(1024 * 1024)
            .ok_or_else(|| "max_total_mb is too large".to_string())?;
        self.keep_days
            .checked_mul(24 * 60 * 60 * 1000)
            .ok_or_else(|| "keep_days is too large".to_string())?;
        Ok(())
    }
}

/// Read settings and preserve any fallback reason so startup can publish it after installing the
/// application log sink. A missing file is the normal first-run state and has no diagnostic.
pub fn load_with_diagnostic() -> (AppSettings, Option<String>) {
    let p = settings_path();
    match std::fs::read_to_string(&p) {
        Ok(text) => match toml::from_str::<AppSettings>(&text) {
            Ok(settings) => match settings.validate() {
                Ok(()) => (settings, None),
                Err(error) => (
                    AppSettings::default(),
                    Some(format!(
                        "settings: {} is invalid, using defaults: {error}",
                        p.display()
                    )),
                ),
            },
            Err(error) => (
                AppSettings::default(),
                Some(format!(
                    "settings: {} failed to parse, using defaults: {error}",
                    p.display()
                )),
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (AppSettings::default(), None)
        }
        Err(error) => (
            AppSettings::default(),
            Some(format!(
                "settings: {} could not be read, using defaults: {error}",
                p.display()
            )),
        ),
    }
}

pub fn load() -> AppSettings {
    load_with_diagnostic().0
}

pub fn save(s: &AppSettings) -> std::io::Result<PathBuf> {
    s.validate()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let text = toml::to_string_pretty(s).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("toml serialize: {e}"),
        )
    })?;
    let p = settings_path();
    let mut staged = crate::fs::staged::Staged::create(&p)?;
    staged.write_all(text.as_bytes())?;
    staged.seal(true)?;
    staged.commit()?;
    Ok(p)
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
