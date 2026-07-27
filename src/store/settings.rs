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
use std::path::{Path, PathBuf};

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

// wholesale migration when the log directory changes

#[derive(Serialize, Deserialize, Default, Debug, Clone, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct MigrateReport {
    #[ts(type = "number")]
    pub moved: u64,
    #[ts(type = "number")]
    pub skipped: u64,
    #[ts(type = "number")]
    pub failed: u64,
    /// Plain-language explanation, pasted straight into the UI
    pub messages: Vec<String>,
}

/// Move the run directories and the index under `old` into `new`. Best-effort throughout:
/// - same directory / old directory missing → return right away
/// - a run directory of that name already in the target → skip (never overwrite someone else's history)
/// - `runs.jsonl` on both sides → merge and rewrite in ts_ms order
/// - cross-volume `rename` fails → fall back to copy + delete (on Windows a cross-volume rename always fails)
pub fn migrate_log_dir(old: &Path, new: &Path) -> MigrateReport {
    let mut r = MigrateReport::default();
    if old == new || !old.is_dir() {
        return r;
    }
    if let Err(e) = std::fs::create_dir_all(new) {
        r.failed += 1;
        r.messages.push(format!("cannot create target directory {}: {e}", new.display()));
        return r;
    }
    let Ok(rd) = std::fs::read_dir(old) else {
        r.failed += 1;
        r.messages.push(format!("cannot read old directory: {}", old.display()));
        return r;
    };
    for e in rd.flatten() {
        let from = e.path();
        let Some(name) = from.file_name().map(|n| n.to_os_string()) else {
            continue;
        };
        let to = new.join(&name);
        if to.exists() {
            if name == crate::foundation::names::RUNLOG_INDEX_FILE {
                merge_index(&from, &to, &mut r);
            } else {
                r.skipped += 1;
            }
            continue;
        }
        move_entry(&from, &to, &mut r);
    }
    if r.moved > 0 || r.failed > 0 {
        r.messages.push(format!("migration done: {} moved, {} skipped, {} failed", r.moved, r.skipped, r.failed));
    }
    r
}

/// Merge two indexes. The index is append-only JSONL, and `latest_by_job` uses "written later = newer" to pick
/// the most recent run — plain concatenation would distort that, so we sort by ts_ms and rewrite.
fn merge_index(from: &Path, into: &Path, r: &mut MigrateReport) {
    let mut lines: Vec<(i64, String)> = Vec::new();
    for p in [from, into] {
        let Ok(t) = std::fs::read_to_string(p) else {
            continue;
        };
        for l in t.lines() {
            let l = l.trim();
            if l.is_empty() {
                continue;
            }
            let ts = serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| v["ts_ms"].as_i64())
                .unwrap_or(0);
            lines.push((ts, l.to_string()));
        }
    }
    lines.sort_by_key(|(ts, _)| *ts);
    let body: String = lines.iter().map(|(_, l)| format!("{l}\n")).collect();
    match std::fs::write(into, body) {
        Ok(_) => {
            let _ = std::fs::remove_file(from);
            r.moved += 1;
        }
        Err(e) => {
            r.failed += 1;
            r.messages.push(format!("index merge failed: {e}"));
        }
    }
}

fn move_entry(from: &Path, to: &Path, r: &mut MigrateReport) {
    // A same-volume rename is atomic and instant; a cross-volume one (i.e. moving to another drive) always fails, so fall through to copy+delete
    if std::fs::rename(from, to).is_ok() {
        r.moved += 1;
        return;
    }
    let copied = if from.is_dir() { copy_dir(from, to) } else { std::fs::copy(from, to).map(|_| ()) };
    match copied {
        Ok(_) => {
            let removed = if from.is_dir() { std::fs::remove_dir_all(from) } else { std::fs::remove_file(from) };
            if let Err(e) = removed {
                // Copy succeeded but the old item won't delete: the data at the new location is complete, so not a failure — just say so
                r.messages.push(format!("copied, but the old item could not be deleted {}: {e}", from.display()));
            }
            r.moved += 1;
        }
        Err(e) => {
            r.failed += 1;
            r.messages.push(format!("move failed {}: {e}", from.display()));
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let (f, t) = (e.path(), to.join(e.file_name()));
        if f.is_dir() {
            copy_dir(&f, &t)?;
        } else {
            std::fs::copy(&f, &t)?;
        }
    }
    Ok(())
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
    fn migrate_moves_dirs_and_skips_collisions() {
        let root = tmp("mig");
        let (old, new) = (root.join("old"), root.join("new"));
        std::fs::create_dir_all(old.join("20260101-000000-a-apply")).unwrap();
        std::fs::write(old.join("20260101-000000-a-apply").join("run.jsonl"), "x\n").unwrap();
        std::fs::create_dir_all(old.join("20260102-000000-b-apply")).unwrap();
        // Same name already in the target → must skip, must not overwrite someone else's history
        std::fs::create_dir_all(new.join("20260102-000000-b-apply")).unwrap();
        std::fs::write(new.join("20260102-000000-b-apply").join("keep"), "mine").unwrap();

        let r = migrate_log_dir(&old, &new);
        assert_eq!(r.moved, 1, "only a moves; b collides by name and is skipped: {r:?}");
        assert_eq!(r.skipped, 1);
        assert_eq!(r.failed, 0);
        assert!(new.join("20260101-000000-a-apply").join("run.jsonl").is_file());
        assert!(new.join("20260102-000000-b-apply").join("keep").is_file(), "the colliding one must not be overwritten");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_merges_index_in_time_order() {
        let root = tmp("idx");
        let (old, new) = (root.join("old"), root.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let idx = crate::foundation::names::RUNLOG_INDEX_FILE;
        std::fs::write(old.join(idx), "{\"ts_ms\":10,\"job\":\"a\"}\n{\"ts_ms\":30,\"job\":\"a\"}\n").unwrap();
        std::fs::write(new.join(idx), "{\"ts_ms\":20,\"job\":\"b\"}\n").unwrap();

        migrate_log_dir(&old, &new);
        let merged = std::fs::read_to_string(new.join(idx)).unwrap();
        let ts: Vec<i64> = merged
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["ts_ms"].as_i64().unwrap())
            .collect();
        assert_eq!(ts, vec![10, 20, 30], "after merging it must be in time order — latest_by_job relies on written-later = newer");
        assert!(!old.join(idx).exists(), "the old index should be deleted after merging");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_same_dir_is_noop() {
        let root = tmp("noop");
        let r = migrate_log_dir(&root, &root);
        assert_eq!((r.moved, r.skipped, r.failed), (0, 0, 0));
        let _ = std::fs::remove_dir_all(&root);
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
