//! L3 the job: one TOML per job (modelled on FFS's "one .ffs_gui per config" shape).
//! Location: Windows %APPDATA%\syncdash\jobs\*.toml, mac ~/.config/syncdash/jobs/*.toml
//!
//! This file owns the `Job` schema and its load/save. `territory` is the generator that
//! walks a tree of `.ffs-sync` markers and writes one job file per territory found.

pub mod territory;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct Job {
    /// mirror | sync | enrich
    pub mode: String,
    pub source: PathBuf,
    pub target: PathBuf,
    /// One source → **many targets** (the original 1:N requirement). Non-empty overrides the single target above:
    /// each target gets its own comparison, its own plan, its own execution (source is scanned once).
    /// mirror/enrich only — sync's N-way merge is version-vector territory, express it as paired jobs; remote jobs don't support multiple targets either.
    #[serde(default)]
    pub targets: Vec<PathBuf>,
    /// Archive of the last sync, for sync mode; refreshed automatically after a successful apply
    #[serde(default)]
    pub archive: Option<PathBuf>,
    /// include allowlist (FFS filter syntax; empty = `*`, everything)
    #[serde(default)]
    pub include: Vec<String>,
    /// Extra excludes (FFS filter syntax, e.g. `big_temp/`, `*.log`; a leading `*` means any depth;
    /// the default junk / rebuildable excludes are already built in).
    ///
    /// Do not put a mask's star-slash sequence on this line: ts-rs copies this doc verbatim into the
    /// generated JSDoc, and those two characters would end the comment block early, yielding invalid .ts.
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub no_hash: bool,
    /// Rigor-level **shortcut preset**: quick | fast | standard | paranoid | custom.
    /// A preset is just a macro over the four detail knobs below; a detail field with a value **overrides** the preset's matching axis (the UI writes all four explicitly on save).
    #[serde(default = "default_rigor")]
    pub rigor: String,
    /// Content evidence: none (0 reads, metadata only) | sampled (sampling window: size + 256KB each at head/middle/tail) | full (whole-file BLAKE3)
    #[serde(default)]
    pub evidence: Option<String>,
    /// Whether to trust the (path,size,mtime) hash cache (unchanged faces reuse last round's result instead of really reading)
    #[serde(default)]
    pub use_cache: Option<bool>,
    /// Disagreement escalation: sampled digests equal but |Δmtime|>2s → re-verify both sides in full before ruling (only meaningful with evidence=sampled)
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
    /// Remote pipeline (optional): once set, run goes over ssh — the remote scans on its own disk (no slow hashing over UNC) plus ship-the-package execution
    #[serde(default)]
    pub remote_host: Option<String>,
    /// Remote root path (the remote machine's own local path, e.g. /Users/xxx/Code/...)
    #[serde(default)]
    pub remote_root: Option<String>,
    /// Path to the remote syncdash executable (defaults to assuming it is on PATH)
    #[serde(default)]
    pub remote_exe: Option<String>,

    // v0.9 safety nets and new capabilities
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
    /// OS-junk exclude preset: auto (both the Win and Mac sets, the default — cross-machine syncs must guard both) | windows | mac | off.
    /// The excluded count always shows in the UI's "⚠ Excluded", never silently
    #[serde(default = "default_os_excludes")]
    pub os_excludes: String,
    /// Exclude rebuildable dev artifacts (.git/node_modules/target/build/dist/venv…).
    /// **Off by default — .git is a normal tree too**; code-sync jobs turn it on explicitly (the cs-* jobs from gen-jobs already do)
    #[serde(default)]
    pub dev_excludes: bool,
    /// Parallel width for the Copy/Update phase (1 = sequential). Defaults to 4; clamped to 1..=16
    #[serde(default)]
    #[ts(type = "number | null")]
    pub parallel: Option<usize>,
    /// M6 scheduled scan: compare automatically every N seconds (None = off). Second-level intervals = "near real-time"; use ≥30 for UNC targets
    #[serde(default)]
    #[ts(type = "number | null")]
    pub watch_interval_secs: Option<u64>,
    /// Apply automatically when watch finds differences (default false = notify only, touch nothing)
    #[serde(default)]
    pub watch_auto_apply: bool,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            mode: "mirror".into(),
            source: PathBuf::new(),
            target: PathBuf::new(),
            targets: Vec::new(),
            archive: None,
            include: Vec::new(),
            exclude: Vec::new(),
            no_hash: false,
            rigor: default_rigor(),
            evidence: None,
            use_cache: None,
            escalate: None,
            verify_writes: None,
            case_sensitive: false,
            symlinks: default_symlinks(),
            versioning: false,
            remote_host: None,
            remote_root: None,
            remote_exe: None,
            require_marker: false,
            min_free_pct: default_min_free(),
            max_delete_ratio: default_max_delete_ratio(),
            fsync: true,
            on_conflict: default_conflict(),
            max_conflicts: default_max_conflicts(),
            sync_mode: false,
            deletable: Vec::new(),
            delta: false,
            os_excludes: default_os_excludes(),
            dev_excludes: false,
            parallel: None,
            watch_interval_secs: None,
            watch_auto_apply: false,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_os_excludes() -> String {
    "auto".into()
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

impl Job {
    /// The effective target list: `targets` when non-empty, otherwise fall back to the single `target`
    pub fn target_list(&self) -> Vec<PathBuf> {
        if self.targets.is_empty() {
            vec![self.target.clone()]
        } else {
            self.targets.clone()
        }
    }

    /// Validity of multiple targets (sync / remote do not support 1:N — the error must be clear before comparing)
    pub fn validate_multi_target(&self) -> Result<(), String> {
        if self.targets.len() > 1 {
            if self.mode == "sync" {
                return Err("sync mode does not support multiple targets (N-way merge needs version-vector attribution) — use paired sync jobs instead (hub-and-spoke)".into());
            }
            if self.remote_host.is_some() {
                return Err("remote jobs do not support multiple targets yet".into());
            }
        }
        Ok(())
    }

    /// Derive a "single-target view" of the job: the engine's / desktop's single pipeline is reused as-is
    pub fn for_target(&self, t: &std::path::Path) -> Job {
        let mut j = self.clone();
        j.target = t.to_path_buf();
        j.targets = Vec::new();
        j
    }
}

/// The **resolved** rigor level: the preset lays the base, the four detail Options override it.
/// Single source of truth — scan_opts / apply_opts / the disagreement-escalation gate all read from here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RigorResolved {
    pub hash: bool,
    pub sampled: bool,
    pub use_cache: bool,
    pub escalate: bool,
    pub verify_writes: bool,
}

impl Job {
    /// preset → baseline; a detail field with a value overrides its axis; `no_hash` (legacy field) forces hashing off last
    pub fn rigor_resolved(&self) -> RigorResolved {
        let (hash, sampled, use_cache, escalate, verify) = match self.rigor.as_str() {
            "quick" => (false, false, false, false, false),
            "fast" => (true, true, true, true, false),
            "paranoid" => (true, false, false, false, true),
            _ => (true, true, false, true, true), // standard / custom baseline
        };
        let mut r = RigorResolved { hash, sampled, use_cache, escalate, verify_writes: verify };
        if let Some(e) = self.evidence.as_deref() {
            match e {
                "none" => {
                    r.hash = false;
                    r.sampled = false;
                }
                "full" => {
                    r.hash = true;
                    r.sampled = false;
                }
                _ => {
                    r.hash = true;
                    r.sampled = true; // sampled
                }
            }
        }
        if let Some(v) = self.use_cache {
            r.use_cache = v;
        }
        if let Some(v) = self.escalate {
            r.escalate = v;
        }
        if let Some(v) = self.verify_writes {
            r.verify_writes = v;
        }
        if self.no_hash {
            r.hash = false;
        }
        r
    }

    pub fn guards(&self, acknowledged: bool) -> crate::pipeline::guard::Guards {
        crate::pipeline::guard::Guards {
            require_marker: self.require_marker,
            min_free_pct: self.min_free_pct,
            max_delete_ratio: self.max_delete_ratio,
            acknowledged,
        }
    }

    pub fn compare_opts(&self) -> crate::pipeline::compare::CompareOptions {
        crate::pipeline::compare::CompareOptions {
            case_insensitive: !self.case_sensitive,
            conflict: match self.on_conflict.as_str() {
                "copy" => crate::pipeline::compare::ConflictPolicy::Copy,
                "newer" => crate::pipeline::compare::ConflictPolicy::Newer,
                _ => crate::pipeline::compare::ConflictPolicy::Report,
            },
            sync_mode: self.sync_mode,
            max_conflicts: self.max_conflicts,
        }
    }

    pub fn apply_opts(&self, trash: Option<PathBuf>, verbose: bool) -> crate::pipeline::apply::ApplyOptions {
        crate::pipeline::apply::ApplyOptions {
            dry_run: false,
            trash,
            verbose,
            // Verify-after-write reads the resolved knob (on by default in the standard/paranoid presets; a detail knob can override)
            verify: self.rigor_resolved().verify_writes,
            versioning: self.versioning,
            fsync: self.fsync,
            filter: Some(crate::pipeline::filter::PathFilter::build_full_opt(&self.include, &self.exclude, &self.deletable, &self.os_excludes, self.dev_excludes)),
            delta: self.delta,
            parallel: self.parallel.unwrap_or(4).clamp(1, 16),
        }
    }
}

fn default_rigor() -> String {
    "standard".into()
}

fn default_symlinks() -> String {
    "exclude".into()
}

pub fn load(name_or_path: &str) -> std::io::Result<(String, Job)> {
    let p = PathBuf::from(name_or_path);
    let path = if p.is_file() {
        p
    } else {
        let cand = crate::foundation::dirs::jobs_dir().join(format!("{name_or_path}.toml"));
        if !cand.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("job not found: {name_or_path} (looked at {})", cand.display()),
            ));
        }
        cand
    };
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let text = std::fs::read_to_string(&path)?;
    let job: Job = toml::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad job file {}: {e}", path.display())))?;
    Ok((name, job))
}

pub fn load_all() -> Vec<(String, Job)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(crate::foundation::dirs::jobs_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "toml").unwrap_or(false) {
                if let Ok(pair) = load(&p.to_string_lossy()) {
                    out.push(pair);
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Save a job (used by the GUI editor). Returns the file path.
pub fn save_job(name: &str, job: &Job) -> std::io::Result<PathBuf> {
    let dir = crate::foundation::dirs::jobs_dir();
    std::fs::create_dir_all(&dir)?;
    let text = toml::to_string_pretty(job)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("toml serialize: {e}")))?;
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(&path, text)?;
    Ok(path)
}

pub fn delete_job(name: &str) -> std::io::Result<()> {
    std::fs::remove_file(crate::foundation::dirs::jobs_dir().join(format!("{name}.toml")))
}

pub const SAMPLE: &str = r#"# %APPDATA%\syncdash\jobs\<name>.toml — one file, one job
mode = "mirror"            # mirror | sync | enrich
source = 'D:\some\dir'
target = '\\host\share\dir'
# archive = 'C:\Users\me\AppData\Roaming\syncdash\archive\<name>.jsonl'   # for sync mode
# include = ['*']                       # FFS filter-syntax allowlist (empty = everything)
# exclude = ['*/big_temp/', '*/*.log']  # FFS syntax; the default junk/rebuildable excludes are built in
# rigor = "standard"                    # shortcut preset: quick | fast | standard | paranoid | custom
# --- rigor detail knobs (a value here overrides the preset's axis; the UI writes them all explicitly) ---
# evidence = "sampled"                  # content evidence: none (0 reads) | sampled (256KB each at head/middle/tail) | full (whole file)
# use_cache = false                     # trust the (path,size,mtime) cache? true in fast; false from standard up = a real read every round
# escalate = true                       # disagreement escalation: digests equal but mtime differs >2s -> re-verify both sides in full
# verify_writes = true                  # verify after write: hash of the copy stream vs a re-read from disk
# case_sensitive = false                # case-insensitive by default (the NTFS/APFS default)
# symlinks = "exclude"                  # exclude | direct (sync the link itself)
# versioning = true                     # deleted/overwritten files go into each root's .version_syncDash/
#                                       # (browse and recover with syncdash versions / restore; the local trash by default)
# no_hash = false
#
# --- safety gates (v0.9) ---
# require_marker = true                 # both roots need .syncdash-root before anything is touched
#                                       # (`syncdash mark <root>` writes it; stops an unmounted share from looking like an empty directory)
# min_free_pct = 0.01                   # minimum free ratio to leave after writing; 0 disables
# max_delete_ratio = 0.5                # refuse to run when one side's deletion share exceeds this (--i-know allows it through); 0 disables
# fsync = true                          # fsync the temp file before renaming; turn off if SMB makes it too slow (at your own risk)
#
# --- conflicts and permissions ---
# on_conflict = "report"                # report (default, report only) | copy (the loser is kept as a .sync-conflict copy) | newer
# max_conflicts = 5                     # with on_conflict="copy", how many copies to keep per file (-1 = unlimited)
# sync_mode = false                     # sync unix permission bits (only meaningful when both sides are unix)
#
# os_excludes = "auto"                  # OS-junk preset: auto (both Win and Mac, the default) | windows | mac | off
# dev_excludes = false                  # exclude dev artifacts (.git/node_modules/target…). Off by default — .git is a normal tree too;
#                                       # set true for code-sync jobs. The excluded count always shows in the UI's "⚠ Excluded", never silently
#
# --- filter extensions ---
# exclude = ['*/*.log', '!*/audit.log'] # a `!` prefix = exception, beats every other exclude
# deletable = ['*/node_modules/']       # not synced, but may go along when a parent directory is deleted (syncthing's (?d))
#
# --- delta and parallelism ---
# delta = true                          # big files on local/mounted disks written chunk-wise; pays off for SMB uploads, a wash on symmetric links
# parallel = 4                          # Copy/Update parallel width (1 = sequential; over SMB 2-4 streams basically saturate the uplink)
#
# --- watch (M6 scheduled scan) ---
# watch_interval_secs = 30              # compare automatically every N seconds; the hash cache means an unchanged tree only pays the walk
# watch_auto_apply = false              # apply automatically on differences (notify only by default)
#
# Remote pipeline (optional): the remote scans on its own disk (fast), the target side is packed and shipped over ssh to execute
# remote_host = 'mac'
# remote_root = '/Users/xxx/Code/some/dir'
# remote_exe = '~/Code/Utilities/SyncDash/target/release/syncdash'
"#;

#[cfg(test)]
mod rigor_tests {
    use super::*;

    fn job(rigor: &str) -> Job {
        Job { rigor: rigor.into(), ..Default::default() }
    }

    #[test]
    fn presets_map_to_expected_knobs() {
        let q = job("quick").rigor_resolved();
        assert!(!q.hash && !q.use_cache && !q.verify_writes);
        let f = job("fast").rigor_resolved();
        assert!(f.hash && f.sampled && f.use_cache && f.escalate && !f.verify_writes);
        let s = job("standard").rigor_resolved();
        assert!(s.hash && s.sampled && !s.use_cache && s.escalate && s.verify_writes);
        let p = job("paranoid").rigor_resolved();
        assert!(p.hash && !p.sampled && !p.use_cache && p.verify_writes);
    }

    #[test]
    fn detail_overrides_beat_preset() {
        let mut j = job("fast");
        j.evidence = Some("full".into());
        j.use_cache = Some(false);
        j.verify_writes = Some(true);
        let r = j.rigor_resolved();
        assert!(r.hash && !r.sampled && !r.use_cache && r.verify_writes);
        // the custom preset takes the standard baseline, then gets overridden by details
        let mut c = job("custom");
        c.evidence = Some("none".into());
        let rc = c.rigor_resolved();
        assert!(!rc.hash);
        assert!(rc.verify_writes, "custom base inherits standard verify");
        // the legacy no_hash field forces hashing off last
        let mut n = job("paranoid");
        n.no_hash = true;
        assert!(!n.rigor_resolved().hash);
    }
}
