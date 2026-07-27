//! Every file and directory name SyncDash itself writes to disk.
//!
//! They live here because of one cross-file constraint: `filter` must exclude these by name, while
//! the writers are atomic / lock / preflight / version / runlog. With the names scattered, a rename
//! anywhere silently disables the filter — once `.syncdash-root` is synced to the far side, an
//! unmounted empty directory grows a marker too, and the mount-point guard fails entirely.

/// Prefix for same-directory temp files. The real name is `{TEMP_PREFIX}{basename}.{pid}`.
pub const TEMP_PREFIX: &str = ".syncdash.tmp.";

/// Maximum temp-file lifetime; anything older counts as debris from a previous crash.
pub const TEMP_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;


/// Root heartbeat lock, guards against two machines applying concurrently.
pub const LOCK_NAME: &str = ".syncdash.lock";

/// Mount-point marker (syncthing's `.stfolder` equivalent). Used together with `require_marker`.
pub const MARKER_NAME: &str = ".syncdash-root";

/// Versioning store directory (inside the root).
pub const VERSION_STORE_DIR: &str = ".version_syncDash";

/// The tool's own directory (caches and the like).
pub const APP_DIR: &str = ".syncdash";


/// Run-overview index (one `RunRecord` per line).
pub const RUNLOG_INDEX_FILE: &str = "runs.jsonl";

/// Summary of a single run.
pub const RUNLOG_SUMMARY_FILE: &str = "summary.json";

/// Plan snapshot of a single run.
pub const RUNLOG_PLAN_FILE: &str = "plan.jsonl";

/// The three event-stream artifacts.
pub const RUNLOG_RUN_FILE: &str = "run.jsonl";
pub const RUNLOG_ERRORS_FILE: &str = "errors.jsonl";
pub const RUNLOG_ITEMS_FILE: &str = "items.jsonl";

/// Process-level application log.
pub const APP_LOG_FILE: &str = "app.jsonl";


/// Conflict-copy infix: `report.pdf` → `report.sync-conflict-<ts>-<host>.pdf`.
pub const CONFLICT_INFIX: &str = ".sync-conflict-";


/// The tool's own metadata. Excluded unconditionally; no tier lets it through.
pub fn self_excludes() -> Vec<String> {
    vec![
        format!("*/{APP_DIR}/"),
        format!("*/{VERSION_STORE_DIR}/"),
        format!("*/{LOCK_NAME}"),
        format!("*/{TEMP_PREFIX}*"),
        format!("*/{MARKER_NAME}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_excludes_cover_every_artifact_we_write() {
        let v = self_excludes();
        // These are the regression gate for "the filter must recognize our own files": lose any one
        // of them and the tool's metadata gets treated as syncable content and shipped to the far side.
        assert!(v.iter().any(|s| s.contains(APP_DIR)));
        assert!(v.iter().any(|s| s.contains(VERSION_STORE_DIR)));
        assert!(v.iter().any(|s| s.contains(LOCK_NAME)));
        assert!(v.iter().any(|s| s.contains(TEMP_PREFIX)));
        assert!(v.iter().any(|s| s.contains(MARKER_NAME)));
        assert_eq!(v.len(), 5);
    }

    #[test]
    fn artifact_names_are_distinct() {
        let all = [
            RUNLOG_INDEX_FILE, RUNLOG_SUMMARY_FILE, RUNLOG_PLAN_FILE,
            RUNLOG_RUN_FILE, RUNLOG_ERRORS_FILE, RUNLOG_ITEMS_FILE, APP_LOG_FILE,
        ];
        let mut sorted = all.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "artifact names inside a run directory must not collide");
    }
}
