//! L0 the event vocabulary: what the engine can say about a run.
//!
//! Vocabulary, not machinery — the same reason `plan` lives here. `settings` needs `LogLevel`
//! for its one config field, and it is a service module that must not depend on the
//! observability stack; with these enums living in `obs::progress` that single import was
//! enough to make `obs` and `store` mutually dependent.
//!
//! These types are exported to TypeScript by ts-rs, so they are the IPC contract as well:
//! `ProgressEvent` is a discriminated union tagged on `kind`.
//!
//! The transport (sinks, the registry) and the control plane (cancel/pause) stay in `obs`.

use serde::{Deserialize, Serialize};

/// Log level. Division of labour with the `Error` event: `Error` is a structured **single-op failure**
/// (carrying path/action/side, naturally a line in the error detail), `Log` is pipeline narration
/// (remote probe results, delta downgrades, lock takeover…) — the sink for those in-library `eprintln!`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// What actually became of one op — the core field of the execution detail (items.jsonl).
/// Today this information lives only in the four branches of `apply::record` and dies with that function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
#[serde(rename_all = "lowercase")]
pub enum ItemOutcome {
    Ok,
    /// Kept because the directory was not empty (protecting filtered-out files is correct, but it must leave a trace)
    Kept,
    /// The user cancelled; this one never got its turn
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    ScanSource,
    ScanTarget,
    Compare,
    Apply,
    Pack,
    Ship,
    Verify,
    /// The archive rescan after a successful apply — a long phase that is completely invisible today
    Refresh,
}

#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// Entering a phase. totals of 0 = not yet known. label = human context (root path, ssh:host…)
    PhaseStart {
        phase: Phase,
        #[ts(type = "number")]
        ts_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[ts(type = "number")]
        items_total: u64,
        #[ts(type = "number")]
        bytes_total: u64,
    },
    /// Mid-phase refinement of the totals (scan: after the walk, before hashing starts)
    Totals {
        phase: Phase,
        #[ts(type = "number")]
        ts_ms: u64,
        #[ts(type = "number")]
        items_total: u64,
        #[ts(type = "number")]
        bytes_total: u64,
    },
    /// Counter snapshot. Emitted at file and chunk boundaries alike; throttling is the sink's job
    Progress {
        phase: Phase,
        #[ts(type = "number")]
        ts_ms: u64,
        #[ts(type = "number")]
        items_done: u64,
        #[ts(type = "number")]
        items_total: u64,
        #[ts(type = "number")]
        bytes_done: u64,
        #[ts(type = "number")]
        bytes_total: u64,
        current_path: String,
    },
    /// One failure record (errors never abort the run — FFS accumulate semantics; in a windowed
    /// desktop build this is where an error first becomes visible)
    Error {
        phase: Phase,
        #[ts(type = "number")]
        ts_ms: u64,
        path: String,
        action: String,
        side: String,
        message: String,
    },
    /// Pipeline narration. `scope` = module name (run / pack / lock…); the panel groups and filters by it.
    /// In a windowed desktop build stderr goes nowhere, so this is the only outlet those lines have.
    Log {
        #[ts(type = "number")]
        ts_ms: u64,
        level: LogLevel,
        scope: String,
        message: String,
    },
    /// What actually became of one op (a line of the execution detail). Division of labour with `Progress`:
    /// Progress is "where are we" (throttling drops frames), ItemResult is "did this one make it" (not one may be lost).
    ItemResult {
        #[ts(type = "number")]
        ts_ms: u64,
        path: String,
        action: String,
        side: String,
        outcome: ItemOutcome,
        #[ts(type = "number")]
        bytes: u64,
        #[ts(type = "number")]
        ms: u64,
    },
    Paused {
        #[ts(type = "number")]
        ts_ms: u64,
    },
    Resumed {
        #[ts(type = "number")]
        ts_ms: u64,
        #[ts(type = "number")]
        paused_ms: u64,
    },
    /// Terminal summary of an apply-class run
    Summary {
        #[ts(type = "number")]
        ts_ms: u64,
        #[ts(type = "number")]
        done: u64,
        #[ts(type = "number")]
        skipped: u64,
        #[ts(type = "number")]
        errors: u64,
        #[ts(type = "number")]
        bytes_done: u64,
        #[ts(type = "number")]
        elapsed_ms: u64,
        #[ts(type = "number")]
        paused_ms: u64,
        cancelled: bool,
    },
}
