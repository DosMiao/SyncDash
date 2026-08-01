//! Moving the run-log directory when `settings.log_dir` changes.
//!
//! Split out of `settings` because it is a cross-volume directory mover, not configuration: it was
//! 127 of that module's 261 production lines and shared nothing with them but the field that
//! triggers it.
//!
//! Best-effort throughout, and never destructive: a run directory whose name already exists on the
//! far side is skipped rather than overwritten, because the two are different machines' histories
//! and neither is authoritative.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

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
                merge_jsonl(&from, &to, "run index", &mut r);
            } else if name == crate::foundation::names::APP_LOG_FILE {
                merge_jsonl(&from, &to, "application log", &mut r);
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

/// Merge two timestamped JSONL streams. The run index is append-only and `latest_by_job` uses
/// "written later = newer"; the app log is also easier to audit in chronological order.
fn merge_jsonl(from: &Path, into: &Path, label: &str, r: &mut MigrateReport) {
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
    let rewrite = (|| -> std::io::Result<()> {
        let mut staged = crate::fs::staged::Staged::create(into)?;
        staged.write_all(body.as_bytes())?;
        staged.seal(true)?;
        staged.commit()
    })();
    match rewrite {
        Ok(_) => {
            let _ = std::fs::remove_file(from);
            r.moved += 1;
        }
        Err(e) => {
            r.failed += 1;
            r.messages.push(format!("{label} merge failed: {e}"));
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
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("syncdash-mig-{tag}-{}", crate::foundation::time::now_ms()));
        std::fs::create_dir_all(&p).unwrap();
        p
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
    fn migration_merges_the_opened_application_log_in_timestamp_order() {
        let root = tmp("app-log");
        let (old, new) = (root.join("old"), root.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let name = crate::foundation::names::APP_LOG_FILE;
        std::fs::write(old.join(name), "{\"ts_ms\":30}\n{\"ts_ms\":10}\n").unwrap();
        std::fs::write(new.join(name), "{\"ts_ms\":20}\n").unwrap();

        let report = migrate_log_dir(&old, &new);

        assert_eq!(report.failed, 0);
        let text = std::fs::read_to_string(new.join(name)).unwrap();
        let times: Vec<i64> = text
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["ts_ms"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert_eq!(times, vec![10, 20, 30]);
        let _ = std::fs::remove_dir_all(root);
    }
}
