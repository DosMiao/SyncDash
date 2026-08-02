//! User-selected comparison policy.

use serde::{Deserialize, Serialize};

use crate::model::plan::MTIME_SLACK_MS;

/// Conflict handling policy. The default reports conflicts without arbitration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Report only; a human handles it.
    Report,
    /// Preserve the loser as `<name>.sync-conflict-<ts>-<host><ext>`.
    Copy,
    /// Newer mtime wins; the older one is overwritten without a conflict copy.
    Newer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareOptions {
    /// Default true: NTFS and APFS are both case-insensitive by default.
    pub case_insensitive: bool,
    /// Conflict policy.
    pub conflict: ConflictPolicy,
    /// Sync Unix permission bits when both sides support them.
    pub sync_mode: bool,
    /// Maximum retained conflict copies per file (`-1` means unlimited).
    pub max_conflicts: i32,
    /// Hashless mtime equality window in milliseconds.
    pub mtime_window_ms: i64,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            case_insensitive: true,
            conflict: ConflictPolicy::Report,
            sync_mode: false,
            max_conflicts: 5,
            mtime_window_ms: MTIME_SLACK_MS,
        }
    }
}
