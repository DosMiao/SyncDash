//! User-selected comparison policy.

use serde::{Deserialize, Serialize};

/// The floor for how far apart two mtimes may be and still count as the same instant.
///
/// FAT stores timestamps at 2-second granularity and SMB shares round-trip through it, so a file
/// copied between two such volumes comes back with a time that differs from its source by up to
/// two seconds without anything having changed. Without this window every such file would compare
/// as modified on every run.
///
/// It is a floor, not the window: `run::local::compare` raises `CompareOptions::mtime_window_ms`
/// to the coarser of the two backends' declared mtime precision before comparing, because an FTP
/// LIST root reports whole minutes. The widened value is what the comparison used and what its
/// result publishes; a reader that wants to judge one comparison's rows must read that, not this.
///
/// This is a knob on how a comparison is made, not part of the artifact it emits — it appears in
/// no PlanHeader, no Op and no plan JSONL — so it lives beside the `CompareOptions` default it
/// supplies rather than in the plan format module.
pub const MTIME_SLACK_MS: i64 = 2000;

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
