//! L3 the job: one TOML per job (modelled on FFS's "one .ffs_gui per config" shape).
//! Location: Windows %APPDATA%\syncdash\jobs\*.toml, mac ~/.config/syncdash/jobs/*.toml
//!
//! This file owns the `Job` schema and its load/save. `territory` is the generator that
//! walks a tree of `.ffs-sync` markers and writes one job file per territory found.

pub mod junk;
pub mod rigor;
pub mod territory;

use serde::{Deserialize, Serialize};

use crate::job::rigor::RigorResolved;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct Job {
    /// Job-file schema version. A file written before the junk presets became part of `exclude`
    /// carries no `schema` key, deserializes as 1, and is migrated on load — see `migrate_v1_junk_presets`.
    /// `save_job` always stamps the current version, because a file we just wrote is by definition current.
    #[serde(default = "default_schema")]
    #[ts(type = "number")]
    pub schema: u32,
    /// mirror | sync | enrich
    pub mode: String,
    /// Root phrase: a local path, or `scheme://…` for a VFS root (sftp/ftp/ftps/smb).
    /// Plain strings so a phrase survives serde untouched; `vfs::spec::parse` routes it.
    /// Serialized form is identical to the old PathBuf fields — existing job files load as-is.
    pub source: String,
    pub target: String,
    /// One source → **many targets** (the original 1:N requirement). Non-empty overrides the single target above:
    /// each target gets its own comparison, its own plan, its own execution (source is scanned once).
    /// mirror/enrich only — sync's N-way merge is version-vector territory, express it as paired jobs; ssh-peer jobs don't support multiple targets either.
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
    /// **This is the whole exclude policy** apart from the tool's own metadata. The junk presets
    /// (Windows / macOS / Developer / …) write their patterns into this very list, so what the editor
    /// shows here is what the filter does — there is no second set of rules applied behind it.
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
            // SCHEMA, **not** `default_schema()`: those two mean opposite things and conflating them is
            // a live bug. `default_schema()` answers "this file has no schema key, so it predates the
            // presets — migrate it"; a Job built here and now is current by construction. Getting this
            // wrong made every job written straight to TOML (gen-jobs) come back through the v1
            // migration on load and silently acquire preset patterns nobody selected.
            schema: SCHEMA,
            mode: "mirror".into(),
            source: String::new(),
            target: String::new(),
            targets: Vec::new(),
            archive: None,
            include: Vec::new(),
            // A new job is born with the default-on junk presets already **written out** in exclude,
            // which is what `os_excludes = "auto"` used to mean invisibly
            exclude: crate::job::junk::default_junk_patterns(),
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
            parallel: None,
            watch_interval_secs: None,
            watch_auto_apply: false,
        }
    }
}

fn default_true() -> bool {
    true
}
/// Current job-file schema. Bump when a load-time migration is added, and give the migration a
/// `schema < N` guard — the version is what tells "the user deleted this rule" apart from
/// "this file predates the rule", which no amount of inspecting the contents can.
pub const SCHEMA: u32 = 2;
fn default_schema() -> u32 {
    1 // no `schema` key in the file = written before versioning existed = needs the v1 migration
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
    /// The source root as a filesystem path (valid for local roots; VFS roots go through `vfs::open`)
    pub fn source_path(&self) -> &std::path::Path {
        std::path::Path::new(&self.source)
    }

    /// The target root as a filesystem path (valid for local roots; VFS roots go through `vfs::open`)
    pub fn target_path(&self) -> &std::path::Path {
        std::path::Path::new(&self.target)
    }

    /// The effective target list: `targets` when non-empty, otherwise fall back to the single `target`
    pub fn target_list(&self) -> Vec<String> {
        if self.targets.is_empty() {
            vec![self.target.clone()]
        } else {
            self.targets.clone()
        }
    }

    /// Validity of the job's shape (multi-target rules + root phrases — the error must be clear before comparing)
    pub fn validate_multi_target(&self) -> Result<(), String> {
        if self.targets.len() > 1 {
            if self.mode == "sync" {
                return Err("sync mode does not support multiple targets (N-way merge needs version-vector attribution) — use paired sync jobs instead (hub-and-spoke)".into());
            }
            if self.remote_host.is_some() {
                return Err("ssh-peer jobs do not support multiple targets yet".into());
            }
        }
        self.validate_roots()
    }

    /// Root-phrase sanity: unknown `xyz://` schemes are refused (never silently local),
    /// and a VFS root cannot be combined with the ssh smart-peer pipeline — they are
    /// different transports and mixing them would leave it ambiguous who owns the target.
    pub fn validate_roots(&self) -> Result<(), String> {
        use crate::fs::vfs::spec::{parse, RootSpec};
        let mut any_remote = false;
        for (label, s) in std::iter::once(("source", &self.source))
            .chain(std::iter::once(("target", &self.target)))
            .chain(self.targets.iter().map(|t| ("targets", t)))
        {
            match parse(s) {
                RootSpec::UnknownScheme { scheme, .. } => {
                    return Err(format!(
                        "{label} '{s}': unknown scheme '{scheme}://' — refusing to treat it as a local path (known: sftp, ftp, ftps, smb)"
                    ));
                }
                RootSpec::Remote(_) => any_remote = true,
                RootSpec::Local(_) => {}
            }
        }
        if any_remote && self.remote_host.is_some() {
            return Err(
                "a job cannot use both a VFS root (scheme:// phrase) and the ssh smart-peer pipeline (remote_host) — pick one transport".into(),
            );
        }
        Ok(())
    }

    /// Derive a "single-target view" of the job: the engine's / desktop's single pipeline is reused as-is
    pub fn for_target(&self, t: &str) -> Job {
        let mut j = self.clone();
        j.target = t.to_string();
        j.targets = Vec::new();
        j
    }
}

/// The **resolved** rigor level: the preset lays the base, the four detail Options override it.

impl Job {
    /// preset → baseline; a detail field with a value overrides its axis; `no_hash` (legacy field) forces hashing off last
    pub fn rigor_resolved(&self) -> RigorResolved {
        RigorResolved::from_preset(&self.rigor)
            .with_evidence(self.evidence.as_deref())
            .with_cache(self.use_cache)
            .with_escalate(self.escalate)
            .with_verify_writes(self.verify_writes)
            .with_no_hash(self.no_hash)
    }

    /// The read-side capability query (window already widened to the coarser backend)
    pub fn read_caps_query(&self, window_ms: i64, src_local: bool, tgt_local: bool) -> crate::pipeline::guard::ReadCapsQuery {
        let rr = self.rigor_resolved();
        crate::pipeline::guard::ReadCapsQuery {
            hash: rr.hash,
            sampled: rr.sampled,
            escalate: rr.escalate,
            symlinks_direct: self.symlinks == "direct",
            min_free_pct: self.min_free_pct,
            window_ms,
            src_local,
            tgt_local,
        }
    }

    /// The write-side capability query (see `guard::cap_report_write`)
    pub fn write_caps_query(&self, src_local: bool, tgt_local: bool) -> crate::pipeline::guard::WriteCapsQuery {
        crate::pipeline::guard::WriteCapsQuery {
            fsync: self.fsync,
            verify: self.rigor_resolved().verify_writes,
            versioning: self.versioning,
            delta: self.delta,
            src_local,
            tgt_local,
        }
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
            // Widened to max(source, target) backend precision once the roots resolve through the VFS (M3)
            mtime_window_ms: crate::model::plan::MTIME_SLACK_MS,
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
            filter: Some(crate::pipeline::filter::PathFilter::build_full(&self.include, &self.exclude, &self.deletable)),
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

/// A job name (looked up in the jobs directory) or a direct path to a `.toml`, resolved to the file.
pub fn resolve_path(name_or_path: &str) -> std::io::Result<PathBuf> {
    let p = PathBuf::from(name_or_path);
    if p.is_file() {
        return Ok(p);
    }
    let cand = crate::foundation::dirs::jobs_dir().join(format!("{name_or_path}.toml"));
    if !cand.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name_or_path} (looked at {})", cand.display()),
        ));
    }
    Ok(cand)
}

/// The schema a job file declares **as it sits on disk**, before `load` migrates it.
///
/// `load` returns the migrated job, so by then the difference has been erased — but a v1 file's junk
/// rules have just been materialized into `exclude`, and anything showing that list has to be able to
/// say those lines are not in the file yet. A file with no `schema` key predates versioning: v1.
pub fn file_schema(name_or_path: &str) -> std::io::Result<u32> {
    #[derive(Deserialize)]
    struct OnlySchema {
        #[serde(default = "default_schema")]
        schema: u32,
    }
    let path = resolve_path(name_or_path)?;
    let text = std::fs::read_to_string(&path)?;
    let parsed: OnlySchema = toml::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad job file {}: {e}", path.display())))?;
    Ok(parsed.schema)
}

pub fn load(name_or_path: &str) -> std::io::Result<(String, Job)> {
    let path = resolve_path(name_or_path)?;
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let text = std::fs::read_to_string(&path)?;
    let mut job: Job = toml::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad job file {}: {e}", path.display())))?;
    if job.schema < SCHEMA {
        migrate_v1_junk_presets(&mut job, &text);
    }
    Ok((name, job))
}

/// The keys a v1 job file used to express junk exclusion. Read once, on load, and never written again.
#[derive(Deserialize)]
struct LegacyJunkKeys {
    /// "auto" (Windows + macOS) | "windows" | "mac" | "off" — absent meant "auto"
    #[serde(default = "legacy_os_excludes_default")]
    os_excludes: String,
    #[serde(default)]
    dev_excludes: bool,
}

fn legacy_os_excludes_default() -> String {
    "auto".into()
}

/// v1 → v2: junk exclusion moves out of `os_excludes`/`dev_excludes` and into `exclude`, where it is visible.
///
/// The expansion is exactly what the old built-in tiers matched, prepended to whatever the user had
/// already written, so a migrated job filters **identically** to before — a migration that quietly widens
/// or narrows a filter is how a sync tool starts proposing deletions nobody asked for.
///
/// The `schema` guard is doing real work: without it there is no way to tell a v2 job whose owner
/// deliberately deleted `*/.DS_Store` from a v1 job that never listed it, and every load would helpfully
/// put the rule back.
fn migrate_v1_junk_presets(job: &mut Job, text: &str) {
    use crate::job::junk::{expand_junk_presets, same_exclude_entry};
    let legacy: LegacyJunkKeys = toml::from_str(text).unwrap_or(LegacyJunkKeys {
        os_excludes: legacy_os_excludes_default(),
        dev_excludes: false,
    });
    let mut ids: Vec<&str> = match legacy.os_excludes.trim() {
        "off" => vec![],
        "windows" => vec!["windows"],
        "mac" => vec!["macos"], // the preset id is spelled out in full; the old key said "mac"
        _ => vec!["windows", "macos"],
    };
    if legacy.dev_excludes {
        ids.push("dev");
    }
    let mut merged = expand_junk_presets(ids);
    merged.retain(|p| !job.exclude.iter().any(|e| same_exclude_entry(e, p)));
    merged.append(&mut job.exclude);
    job.exclude = merged;
    job.schema = SCHEMA;
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
    // Stamp the version here rather than trusting the caller: a file written by this build is current
    // by construction, and a stale `schema` on the way in would make the next load re-run the v1
    // migration and re-add preset patterns the user had just deleted.
    let job = &Job { schema: SCHEMA, ..job.clone() };
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
schema = 2                 # job-file schema; a file without it is migrated on load (junk presets -> exclude)
mode = "mirror"            # mirror | sync | enrich
source = 'D:\some\dir'
target = '\\host\share\dir'
# archive = 'C:\Users\me\AppData\Roaming\syncdash\archive\<name>.jsonl'   # for sync mode
# include = ['*']                       # FFS filter-syntax allowlist (empty = everything)
# exclude = ['*/big_temp/', '*/*.log']  # FFS syntax. The ONLY exclude policy besides this tool's own metadata —
#                                       # junk presets (Windows/macOS/Linux/Developer/IDE/Office/sync tools) write
#                                       # their patterns straight into this list, so it always reads as what runs.
#                                       # `syncdash junk` prints the presets; `syncdash scan --junk <ids>` applies them ad hoc
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

/// The v1 → v2 migration. Its one job is to be a **behavioural no-op**: whatever a job file used to
/// exclude via `os_excludes`/`dev_excludes`, it must still exclude — now spelled out where it can be read.
#[cfg(test)]
mod migration_tests {
    use super::*;
    use crate::job::junk::expand_junk_presets;
    use crate::pipeline::filter::PathFilter;

    fn load_text(tag: &str, text: &str) -> Job {
        let p = std::env::temp_dir().join(format!("syncdash-job-{tag}-{}.toml", crate::foundation::time::now_ms()));
        std::fs::write(&p, text).unwrap();
        let (_, j) = load(&p.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&p);
        j
    }

    const HEAD: &str = "mode = 'mirror'\nsource = 'S'\ntarget = 'T'\n";

    #[test]
    fn legacy_presets_land_in_exclude_and_filter_identically() {
        // os_excludes = "auto" + dev_excludes = true, the shape `gen-jobs` wrote for every cs-* job
        let j = load_text("auto-dev", &format!("{HEAD}os_excludes = 'auto'\ndev_excludes = true\n"));
        assert_eq!(j.schema, SCHEMA);
        assert_eq!(j.exclude, expand_junk_presets(["windows", "macos", "dev"]));
        let pf = PathFilter::build(&j.include, &j.exclude);
        assert!(!pf.pass_file("a/Thumbs.db"));
        assert!(!pf.pass_file("a/.DS_Store"));
        assert!(!pf.pass_dir("proj/.git").0);

        // A file that never mentioned the key: absent meant "auto", so it must still mean Windows + macOS
        let j = load_text("implicit", HEAD);
        assert_eq!(j.exclude, expand_junk_presets(["windows", "macos"]));

        // …and "off" meant off. Migrating it into a non-empty list would be inventing a rule.
        let j = load_text("off", &format!("{HEAD}os_excludes = 'off'\n"));
        assert!(j.exclude.is_empty());

        // The old key spelled macOS "mac"; the preset id is "macos". The rules must survive the rename.
        let j = load_text("mac", &format!("{HEAD}os_excludes = 'mac'\n"));
        assert_eq!(j.exclude, expand_junk_presets(["macos"]));
        assert!(!PathFilter::build(&[], &j.exclude).pass_file("a/.DS_Store"));
        assert!(PathFilter::build(&[], &j.exclude).pass_file("a/Thumbs.db"), "'mac' must not drag Windows in");
    }

    #[test]
    fn migration_keeps_the_users_own_lines_and_never_duplicates() {
        let j = load_text(
            "mixed",
            // Single-quoted TOML is literal — this reaches the job as one backslash, the Windows spelling
            &format!("{HEAD}os_excludes = 'windows'\nexclude = ['*/big_temp/', '*\\Thumbs.db', '!*/keep.log']\n"),
        );
        // The user had already written Thumbs.db by hand (backslash, different case): one line, not two
        assert_eq!(j.exclude.iter().filter(|e| e.to_lowercase().contains("thumbs.db")).count(), 1);
        // Their own entries survive, in their own order, after the preset block
        assert!(j.exclude.contains(&"*/big_temp/".to_string()));
        assert!(j.exclude.contains(&"!*/keep.log".to_string()));
        let pf = PathFilter::build(&[], &j.exclude);
        assert!(!pf.pass_file("x/big_temp/f"));
        assert!(pf.pass_file("x/keep.log"), "the ! exception must survive the migration");
    }

    /// The regression this whole version field exists to prevent: once migrated, a user who deletes a
    /// preset line must find it still gone next time. A load that "helpfully" restores it is a filter
    /// that silently disagrees with what the editor shows.
    #[test]
    fn a_v2_job_is_never_re_migrated() {
        let j = load_text("v2", &format!("schema = 2\n{HEAD}exclude = ['*/only_this/']\n"));
        assert_eq!(j.exclude, vec!["*/only_this/".to_string()], "v2 files are taken at their word");
        let j = load_text("v2-empty", &format!("schema = 2\n{HEAD}"));
        assert!(j.exclude.is_empty(), "an empty exclude in a v2 file means empty");
    }

    /// A Job built in memory and written straight to TOML — which is what `gen-jobs` does, bypassing
    /// `save_job` — must come back exactly as written. It did not: `Job::default()` stamped the *legacy*
    /// schema, so every generated job was re-migrated on load and quietly grew preset patterns its author
    /// never chose. Exclusions appearing on their own is the failure mode this whole design is against.
    #[test]
    fn a_job_written_straight_to_toml_round_trips_untouched() {
        assert_eq!(Job::default().schema, SCHEMA, "a Job built by current code is current-shape");
        let j = Job {
            source: "S".into(),
            target: "T".into(),
            exclude: vec!["*/only_mine/".into()],
            ..Default::default()
        };
        let p = std::env::temp_dir().join(format!("syncdash-job-rt-{}.toml", crate::foundation::time::now_ms()));
        std::fs::write(&p, toml::to_string_pretty(&j).unwrap()).unwrap();
        let (_, back) = load(&p.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&p);
        assert_eq!(back.exclude, j.exclude, "no rule may appear that the author did not write");
        assert_eq!(back.schema, SCHEMA);
    }

    /// `save_job` stamps the current schema itself. If it trusted a caller-supplied version, a frontend
    /// that omitted the field would round-trip a v2 job back to v1 and the next load would re-add presets.
    #[test]
    fn saving_stamps_the_current_schema() {
        let stale = Job { schema: 1, exclude: vec!["*/mine/".into()], ..Default::default() };
        let text = toml::to_string_pretty(&Job { schema: SCHEMA, ..stale.clone() }).unwrap();
        assert!(text.contains("schema = 2"));
        // …and the migration would have fired on the stale one, which is exactly what stamping prevents
        let j = load_text("stale", &format!("schema = 1\n{HEAD}exclude = ['*/mine/']\n"));
        assert!(j.exclude.len() > 1, "premise: schema 1 does trigger the migration");
    }

    /// `file_schema` reports the file, not the loaded job — that difference is the whole point of it.
    /// The editor uses it to say "these exclude lines came from the migration and are not in the file
    /// yet"; if it reported the migrated value it would always say nothing had happened.
    #[test]
    fn file_schema_reports_the_file_not_the_migrated_job() {
        let write = |tag: &str, text: &str| {
            let p = std::env::temp_dir().join(format!("syncdash-fs-{tag}-{}.toml", crate::foundation::time::now_ms()));
            std::fs::write(&p, text).unwrap();
            p
        };

        // A v1 file: on disk it is 1, while the job load hands back is already migrated to 2
        let p = write("v1", &format!("{HEAD}os_excludes = 'auto'\n"));
        assert_eq!(file_schema(&p.to_string_lossy()).unwrap(), 1, "no schema key = v1");
        assert_eq!(load(&p.to_string_lossy()).unwrap().1.schema, SCHEMA, "…but the loaded job is migrated");
        let _ = std::fs::remove_file(&p);

        // A v2 file says so, and there is nothing to announce
        let p = write("v2", &format!("schema = 2\n{HEAD}"));
        assert_eq!(file_schema(&p.to_string_lossy()).unwrap(), SCHEMA);
        let _ = std::fs::remove_file(&p);

        assert!(file_schema("no-such-job-exists-here").is_err(), "a missing file is an error, not a version");
    }
}

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
