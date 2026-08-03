//! Persisted job schema and the validated single-target execution view.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Current job-file schema. A missing key is schema v1; persistence applies each migration in
/// sequence before exposing a job to callers.
pub const SCHEMA: u32 = 4;

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub struct Job {
    /// Job-file schema version. A missing key means v1; load runs each one-way migration in order,
    /// while every current save stamps `SCHEMA` and serializes only current fields.
    #[serde(default = "default_schema")]
    #[ts(type = "number")]
    pub schema: u32,
    /// Opaque identity of the registered job file. It is assigned once when a job enters the jobs
    /// directory, moves with a rename, and is deliberately excluded from `config_revision`.
    /// An empty value is valid only for an unsaved/default job.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub job_id: String,
    /// mirror | sync | enrich
    pub mode: String,
    /// Root phrase: a local path, or `scheme://…` for a VFS root (sftp/ftp/ftps/smb).
    /// Plain strings so a phrase survives serde untouched; `vfs::spec::parse` routes it.
    pub source: String,
    /// One source → one or more targets. Each target owns its own comparison, plan, and execution.
    /// The persisted current schema requires at least one entry; schema v1–v3 scalar `target`
    /// storage is converted once by `migrate_v3_current_schema` during load.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Archive of the last sync, for sync mode; refreshed automatically after a successful apply
    #[serde(default)]
    pub archive: Option<PathBuf>,
    /// include allowlist (FFS filter syntax; empty = `*`, everything)
    #[serde(default)]
    pub include: Vec<String>,
    /// Excludes (FFS filter syntax, e.g. `big_temp/`, `*.log`; a leading `*` means any depth;
    /// a leading `!` makes the line an exception).
    ///
    /// This is the complete user-visible exclude policy. Junk presets write their patterns here
    /// rather than applying hidden rules.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Rigor-level **shortcut preset**: quick | fast | balanced | standard | paranoid | custom.
    /// A preset is just a macro over the four detail knobs below; a detail field with a value **overrides** the preset's matching axis (the UI writes all four explicitly on save).
    #[serde(default = "default_rigor")]
    pub rigor: String,
    /// Content evidence: none (0 reads, metadata only) | sampled (sampling window: size + 256KB each at head/middle/tail) | full (whole-file BLAKE3)
    #[serde(default)]
    pub evidence: Option<String>,
    /// Whether to trust the (path,size,mtime) hash cache (unchanged faces reuse last round's result instead of really reading)
    #[serde(default)]
    pub use_cache: Option<bool>,
    /// Disagreement escalation: sampled digests equal but the mtimes fall outside the comparison's equality window (≥ 2 s, widened on coarse-timestamp backends) → re-verify both sides in full before ruling (only meaningful with evidence=sampled)
    #[serde(default)]
    pub escalate: Option<bool>,
    /// Verify after write: full blake3 of the copy stream vs a re-read from disk; no rename unless they match
    #[serde(default)]
    pub verify_writes: Option<bool>,
    /// Default false (case-insensitive matching — the NTFS/APFS default); true makes matching case-sensitive
    #[serde(default)]
    pub case_sensitive: bool,
    /// symlink policy: exclude (default, ignore) | direct (sync the link itself, compared by its target string)
    #[serde(default = "default_symlinks")]
    pub symlinks: String,
    /// Versioning (optional): when true, deleted/overwritten files go into each root's .version_syncDash/ (history travels with the data),
    /// paired with `syncdash versions` / `syncdash restore` to browse and recover; false uses the local trash
    #[serde(default)]
    pub versioning: bool,
    /// Require a `.syncdash-root` marker on both roots before touching anything (so an unmounted SMB share isn't treated as an empty directory).
    /// Recommended for new jobs; `syncdash mark <root>` writes the marker.
    #[serde(default)]
    pub require_marker: bool,
    /// Minimum free-disk ratio to keep (0.01 = 1%). 0 disables
    #[serde(default = "default_min_free")]
    pub min_free_pct: f64,
    /// Refuse to run when one side's share of deleted entries exceeds this (0.5 = 50%). 0 or >=1 disables.
    /// A wrong filter, swapped source/target, and an unmounted share all look exactly like this.
    #[serde(default = "default_max_delete_ratio")]
    pub max_delete_ratio: f64,
    /// fsync the temp file before renaming. On by default; turn it off if SMB makes it too slow (at your own risk)
    #[serde(default = "default_true")]
    pub fsync: bool,
    /// Conflict policy: report (default, report only) | copy (the loser is kept as a .sync-conflict copy) | newer (the newer one just overwrites)
    #[serde(default = "default_conflict")]
    pub on_conflict: String,
    /// With on_conflict = "copy", the most conflict copies to keep per file (-1 = unlimited)
    #[serde(default = "default_max_conflicts")]
    pub max_conflicts: i32,
    /// Sync unix permission bits (only meaningful when both sides are unix)
    #[serde(default)]
    pub sync_mode: bool,
    /// These paths don't take part in the sync, but may be removed along with a parent directory (syncthing's `(?d)`)
    #[serde(default)]
    pub deletable: Vec<String>,
    /// Delta updates for local/mounted disks: one extra read of the target buys a lot fewer written bytes.
    /// A net win on SMB/WAN uploads, a wash on symmetric links — hence off by default, enable it per link.
    #[serde(default)]
    pub delta: bool,
    /// Parallel width for the Copy/Update phase (1 = sequential). Defaults to 4; clamped to 1..=16
    #[serde(default)]
    #[ts(type = "number | null")]
    pub parallel: Option<usize>,
    /// AutoScan's maximum full-verification interval in seconds (None = off).
    #[serde(default)]
    #[ts(type = "number | null")]
    pub autoscan_interval_secs: Option<u64>,
    /// Apply an AutoScan result automatically when exact unattended authorization allows it.
    #[serde(default)]
    pub autoscan_auto_apply: bool,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            // New jobs are current; default_schema() is only the serde fallback for unversioned files.
            schema: SCHEMA,
            job_id: String::new(),
            mode: "mirror".into(),
            source: String::new(),
            targets: vec![String::new()],
            archive: None,
            include: Vec::new(),
            // Presets are materialized so the editor and engine share one exclude policy.
            exclude: crate::job::junk::default_junk_patterns(),
            rigor: default_rigor(),
            evidence: None,
            use_cache: None,
            escalate: None,
            verify_writes: None,
            case_sensitive: false,
            symlinks: default_symlinks(),
            versioning: false,
            require_marker: false,
            min_free_pct: default_min_free(),
            max_delete_ratio: default_max_delete_ratio(),
            fsync: true,
            on_conflict: default_conflict(),
            max_conflicts: default_max_conflicts(),
            sync_mode: false,
            deletable: Vec::new(),
            delta: false,
            parallel: None,
            autoscan_interval_secs: None,
            autoscan_auto_apply: false,
        }
    }
}

impl Job {
    /// The source root as a filesystem path (valid for local roots; VFS roots use `vfs::open`).
    pub fn source_path(&self) -> &Path {
        Path::new(&self.source)
    }
}

/// A validated job configuration normalized to exactly one target, plus its index in the persisted
/// job that selected it.
///
/// Phrase-based execution accepts this type instead of `Job`, so no run can accidentally infer a
/// scalar target from a multi-target configuration. Construction stays inside job policy so the
/// validation, bounds, and one-target invariants cannot be bypassed.
#[derive(Clone, Debug)]
pub struct SingleTargetJob {
    pub(super) configuration: Job,
    pub(super) target_index: usize,
}

impl SingleTargetJob {
    pub fn configuration(&self) -> &Job {
        &self.configuration
    }

    pub fn target_index(&self) -> usize {
        self.target_index
    }

    pub fn target(&self) -> &str {
        &self.configuration.targets[0]
    }
}

pub(super) fn default_schema() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

fn default_min_free() -> f64 {
    0.01
}

fn default_max_delete_ratio() -> f64 {
    0.5
}

fn default_conflict() -> String {
    "report".into()
}

fn default_max_conflicts() -> i32 {
    5
}

fn default_rigor() -> String {
    "standard".into()
}

fn default_symlinks() -> String {
    "exclude".into()
}
