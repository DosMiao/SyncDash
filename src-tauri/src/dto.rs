//! The wire types. Every one is exported by ts-rs into `typescript/core/types/generated/`, so the
//! frontend never hand-writes a shape the Rust side owns.

use serde::{Deserialize, Serialize};

use syncdash::model::plan::{Op, PlanHeader};
use syncdash::pipeline::compare;

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobDto {
    pub(crate) name: String,
    pub(crate) mode: String,
    pub(crate) rigor: String,
    pub(crate) source: String,
    pub(crate) target: String,
    pub(crate) has_archive: bool,
    // v0.9 M3: fill in the fields the frontend needs to see (remote badge / versioning marker / filter hints).
    // The host is not among them: it lives in `target` now (`peer://<host>/…`), and shipping a second
    // copy is how the frontend ends up rendering a stale one.
    pub(crate) remote: bool,
    pub(crate) versioning: bool,
    pub(crate) delta: bool,
    #[ts(type = "number | null")]
    pub(crate) parallel: Option<usize>,
    pub(crate) include: Vec<String>,
    pub(crate) exclude: Vec<String>,
    #[ts(type = "number | null")]
    pub(crate) watch_interval_secs: Option<u64>,
    pub(crate) watch_auto_apply: bool,
    /// 1:N: the effective target list (a single-target job = one entry). When >1 the frontend shows the target selector
    pub(crate) targets: Vec<String>,
    pub(crate) config_revision: String,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobDetailDto {
    pub(crate) name: String,
    pub(crate) job: syncdash::job::Job,
    pub(crate) config_revision: String,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobSaveDto {
    pub(crate) name: String,
    pub(crate) config_revision: String,
}

/// Immutable provenance for one successful comparison.
///
/// The frontend carries this value with the plan. Commands that read cached evidence or can write
/// files require an exact match, so changing selection cannot silently reinterpret an old plan.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct CompareOwner {
    #[ts(type = "number")]
    pub(crate) compare_id: u64,
    pub(crate) job_name: String,
    #[ts(type = "number")]
    pub(crate) target_index: usize,
    pub(crate) config_revision: String,
}

#[derive(Serialize, Deserialize, Clone, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct PlanDto {
    pub(crate) header: PlanHeader,
    pub(crate) ops: Vec<Op>,
    /// One entry per op: the size/mtime measured on both sides at compare time (for the table columns and sorting).
    /// Copy rows already carry their sole side's size/mtime in `Op`, so their entry is null and the
    /// frontend reconstructs it. This matters at six figures: two nested JSON objects per copy row
    /// otherwise account for most of WebKit's retained allocations.
    #[serde(default)]
    pub(crate) metas: Vec<Option<compare::evidence::RowMeta>>,
    /// Count/bytes of the files judged equal on both sides (the denominator of "showing X of Y")
    #[serde(default)]
    #[ts(type = "number")]
    pub(crate) equal_count: u64,
    #[serde(default)]
    #[ts(type = "number")]
    pub(crate) equal_bytes: u64,
    pub(crate) owner: CompareOwner,
}

/// One reviewed plan row submitted for preflight/apply. The backend reconstructs the operation from
/// the cached plan; the frontend never gets to submit an independent write instruction.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct SelectedRowDto {
    #[ts(type = "number")]
    pub(crate) index: usize,
    pub(crate) flipped: bool,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct ApplyDto {
    #[ts(type = "number")]
    pub(crate) done: u64,
    #[ts(type = "number")]
    pub(crate) skipped: u64,
    #[ts(type = "number")]
    pub(crate) errors: u64,
    #[ts(type = "number")]
    pub(crate) bytes_copied: u64,
    pub(crate) cancelled: bool,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct PreflightDto {
    pub(crate) ok: bool,
    pub(crate) acknowledgeable: bool,
    pub(crate) blockers: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Serialize, Default, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct PathInfo {
    pub(crate) exists: bool,
    pub(crate) is_dir: bool,
    pub(crate) has_marker: bool,
}

#[derive(Serialize, Default, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct PathVerdict {
    pub(crate) source: PathInfo,
    pub(crate) target: PathInfo,
    /// Plain-language warnings; the editor renders them right under the field
    pub(crate) warnings: Vec<String>,
}

// Snapshot cache (the data source for the "Identical" panel)
//
// compare already walked both sides in full; dropping the snapshots would force the UI to rescan just to glance at the identical items.
// Single-slot cache: every *successful* compare overwrites it; merely changing the selected job does
// not. Two snapshots at the 20k-entry scale run to a dozen-odd MB.
#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct SamePage {
    #[ts(type = "number")]
    pub(crate) total: u64,
    pub(crate) rows: Vec<compare::evidence::SameRow>,
    /// Which job's snapshot is sitting in the cache (on a mismatch the UI prompts for a fresh compare)
    pub(crate) job: String,
}

// Run state (mutual exclusion + cancel/pause handles)
/// The `schema` recorded in the job file **as it sits on disk**, next to the version this build writes.
/// `get_job` returns the migrated job, so by then the difference is gone — but the editor is showing
/// exclude lines the file does not contain yet, and it has to be able to say so.
/// The two differing means the v1 junk keys were expanded into `exclude` on load.
#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobFileSchemaDto {
    #[ts(type = "number")]
    pub(crate) on_disk: u32,
    #[ts(type = "number")]
    pub(crate) current: u32,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JunkPresetDto {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) hint: String,
    pub(crate) patterns: Vec<String>,
    pub(crate) default_on: bool,
}
