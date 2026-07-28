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
            eprintln!("settings: log dir {} unusable, falling back to {}", want.display(), fallback.display());
            return fallback;
        }
        std::env::temp_dir().join("syncdash-logs")
    }

    pub fn logs_compare(&self) -> bool {
        self.log_compare != "off"
    }
}

/// Read the settings. A missing file or a broken one both fall back to the defaults.
pub fn load() -> AppSettings {
    let p = settings_path();
    match std::fs::read_to_string(&p) {
        Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
            eprintln!("settings: {} failed to parse, using defaults: {e}", p.display());
            AppSettings::default()
        }),
        Err(_) => AppSettings::default(),
    }
}

pub fn save(s: &AppSettings) -> std::io::Result<PathBuf> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let text = toml::to_string_pretty(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("toml serialize: {e}")))?;
    let p = settings_path();
    std::fs::write(&p, text)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("syncdash-set-{tag}-{}", crate::foundation::time::now_ms()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        let s = AppSettings { log_dir: "D:\\logs".into(), level: LogLevel::Warn, keep_days: 7, ..Default::default() };
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
    fn resolved_log_dir_falls_back_when_unwritable() {
        // Put a **file** into the setting where a directory belongs: create_dir_all must fail → it has to fall back somewhere writable
        let root = tmp("fallback");
        let f = root.join("not-a-dir");
        std::fs::write(&f, "x").unwrap();
        let s = AppSettings { log_dir: f.display().to_string(), ..Default::default() };
        let got = s.resolved_log_dir();
        assert_ne!(got, f, "must not return a path that cannot be written");
        assert!(got.is_dir(), "the fallback target must actually exist: {}", got.display());
        let _ = std::fs::remove_dir_all(&root);
    }
}
