//! Cross-cutting filename rules and every metadata name SyncDash writes itself.
//!
//! Internal names live here because filters and writers must agree exactly. Windows name faults
//! live here because both compare-time planning and apply-time validation need the same classifier.

use crate::foundation::path::RootRelativePath;

/// Prefix for same-directory temp files. Local names also carry the process and stage IDs.
pub const TEMP_PREFIX: &str = ".syncdash.tmp.";

/// Maximum temp-file lifetime; anything older counts as debris from a previous crash.
pub const TEMP_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;

/// Root lock-ledger anchor and prefix, guards against two machines applying concurrently.
pub const LOCK_NAME: &str = ".syncdash.lock";

/// Mount-point marker (syncthing's `.stfolder` equivalent). Used together with `require_marker`.
pub const MARKER_NAME: &str = ".syncdash-root";

/// Versioning store directory (inside the root).
pub const VERSION_STORE_DIR: &str = ".version_syncDash";

/// The tool's own directory (caches and the like).
pub const APP_DIR: &str = ".syncdash";

/// Run-overview index (one `RunRecord` per line).
pub const RUNLOG_INDEX_FILE: &str = "runs.jsonl";

/// Immutable source retained by the one-way run-index migration.
pub const RUNLOG_LEGACY_INDEX_FILE: &str = "runs.legacy-v1.jsonl";

/// Serializes run-index migration, append, and retention across processes.
pub const RUNLOG_INDEX_LOCK_FILE: &str = ".syncdash.run-index.lock";

/// Commits completion of the one-way run-record schema migration.
pub const RUNLOG_SCHEMA_FILE: &str = "runs.schema";

/// Summary of a single run.
pub const RUNLOG_SUMMARY_FILE: &str = "summary.json";

/// Immutable source retained when a legacy run summary is migrated.
pub const RUNLOG_LEGACY_SUMMARY_FILE: &str = "summary.legacy-v1.json";

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

/// How Windows path parsing prevents a relative path from preserving its intended name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsNameFault {
    /// The path resolves to a different object, so every read and mutation is unsafe.
    Mangled,
    /// Windows rejects creation of this name.
    Rejected,
    /// SyncDash can create the name, but common Windows tools cannot manage it reliably.
    Unusable,
}

impl WindowsNameFault {
    pub fn changes_addressed_path(self) -> bool {
        matches!(self, Self::Mangled)
    }
}

/// Classify a path that cannot be handled faithfully under Windows naming semantics.
///
/// Colons and trailing dots or spaces are address-mangling: operations can report success against
/// a different object. Reserved device names remain addressable through verbatim paths, but are
/// classified separately because Explorer and common Win32 tools cannot manage them reliably.
pub fn windows_name_fault(relative: &str) -> Option<(WindowsNameFault, String)> {
    const RESERVED_DEVICE_NAMES: [&str; 24] = [
        "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
        "LPT9",
    ];

    for segment in relative.split('/') {
        if let Some(stripped) = segment
            .strip_suffix('.')
            .or_else(|| segment.strip_suffix(' '))
        {
            return Some((
                WindowsNameFault::Mangled,
                format!(
                    "mangled: '{segment}' would be silently truncated to '{stripped}' and could overwrite a different file"
                ),
            ));
        }
        if segment.contains(':') {
            return Some((
                WindowsNameFault::Mangled,
                format!(
                    "mangled: '{segment}' would be written into an alternate data stream — the write reports success, but the name disappears from the directory"
                ),
            ));
        }
        if let Some(character) = segment.chars().find(|character| {
            matches!(character, '<' | '>' | '"' | '|' | '?' | '*' | '\\')
                || (*character as u32) < 0x20
        }) {
            return Some((
                WindowsNameFault::Rejected,
                format!("rejected: Windows refuses the character {character:?} in '{segment}'"),
            ));
        }
        let base = segment.split('.').next().unwrap_or("").to_ascii_uppercase();
        if RESERVED_DEVICE_NAMES.contains(&base.as_str()) {
            return Some((
                WindowsNameFault::Unusable,
                format!(
                    "unusable: '{segment}' is a reserved device name — it can be created, but Explorer and most Windows programs cannot open or delete it afterwards"
                ),
            ));
        }
    }
    None
}

/// Whether a validated root-relative path belongs to SyncDash rather than synchronized content.
/// The rule matches `self_excludes`: internal directories and marker names are reserved at every
/// depth, while lock-ledger and staging artifacts reserve their complete name prefixes.
pub fn is_internal_artifact_path(path: &RootRelativePath) -> bool {
    path.as_str().split('/').any(|segment| {
        segment == APP_DIR
            || segment == VERSION_STORE_DIR
            || segment == MARKER_NAME
            || segment.starts_with(LOCK_NAME)
            || segment.starts_with(TEMP_PREFIX)
    })
}

/// The tool's own metadata. Excluded unconditionally; no tier lets it through.
pub fn self_excludes() -> Vec<String> {
    vec![
        format!("*/{APP_DIR}/"),
        format!("*/{VERSION_STORE_DIR}/"),
        format!("*/{LOCK_NAME}*"),
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
            RUNLOG_INDEX_FILE,
            RUNLOG_LEGACY_INDEX_FILE,
            RUNLOG_INDEX_LOCK_FILE,
            RUNLOG_SCHEMA_FILE,
            RUNLOG_SUMMARY_FILE,
            RUNLOG_LEGACY_SUMMARY_FILE,
            RUNLOG_PLAN_FILE,
            RUNLOG_RUN_FILE,
            RUNLOG_ERRORS_FILE,
            RUNLOG_ITEMS_FILE,
            APP_LOG_FILE,
        ];
        let mut sorted = all.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            all.len(),
            "artifact names inside a run directory must not collide"
        );
    }

    #[test]
    fn internal_artifact_paths_match_nested_filter_scope() {
        for path in [
            ".syncdash/cache",
            "nested/.version_syncDash/index.jsonl",
            "nested/.syncdash.lock.claim",
            "nested/.syncdash.tmp.stage",
            "nested/.syncdash-root",
        ] {
            assert!(is_internal_artifact_path(
                &RootRelativePath::try_from(path).unwrap()
            ));
        }
        assert!(!is_internal_artifact_path(
            &RootRelativePath::try_from("nested/syncdash-user.txt").unwrap()
        ));
    }

    #[test]
    fn windows_name_faults_distinguish_addressing_from_usability() {
        let (colon_fault, _) = windows_name_fault("report:2024.pdf").unwrap();
        let (character_fault, _) = windows_name_fault("report?.pdf").unwrap();
        let (reserved_fault, _) = windows_name_fault("reports/CON.txt").unwrap();

        assert!(colon_fault.changes_addressed_path());
        assert_eq!(character_fault, WindowsNameFault::Rejected);
        assert_eq!(reserved_fault, WindowsNameFault::Unusable);
        assert!(windows_name_fault("reports/2024.pdf").is_none());
    }
}
