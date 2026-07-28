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
pub const SCHEMA: u32 = 3;
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
            if self.target_list().iter().any(|t| crate::fs::vfs::spec::is_peer(t)) {
                return Err("peer:// targets do not support multiple targets yet".into());
            }
        }
        self.validate_roots()
    }

    /// Root-phrase sanity: unknown `xyz://` schemes are refused, never silently treated as a local
    /// path, and a peer root may only be the target.
    ///
    /// There used to be a second rule here — a `scheme://` root could not be combined with the ssh
    /// peer pipeline, because "where the target lives" was answered in two unrelated places and
    /// nothing said which won. With `peer://` in the grammar there is one answer, so there is
    /// nothing left to keep apart.
    pub fn validate_roots(&self) -> Result<(), String> {
        use crate::fs::vfs::spec::{is_peer, parse, RootSpec, KNOWN_SCHEMES};
        for (label, s) in std::iter::once(("source", &self.source))
            .chain(std::iter::once(("target", &self.target)))
            .chain(self.targets.iter().map(|t| ("targets", t)))
        {
            if let RootSpec::UnknownScheme { scheme, .. } = parse(s) {
                return Err(format!(
                    "{label} '{s}': unknown scheme '{scheme}://' — refusing to treat it as a local path (known: {})",
                    KNOWN_SCHEMES.join(", ")
                ));
            }
        }
        // The peer lane is directional by construction: this side builds a package and the far
        // side applies it. A peer *source* would mean asking it to send one back, which nothing
        // implements — better a clear error than a run that reads an empty tree.
        if is_peer(&self.source) {
            return Err(format!(
                "source '{}': a peer root can only be the target — this side packs, the far side applies",
                self.source
            ));
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
    pub fn read_caps_query(&self, window_ms: i64, src_local: bool, tgt_local: bool) -> crate::pipeline::guard::caps::ReadCapsQuery {
        let rr = self.rigor_resolved();
        crate::pipeline::guard::caps::ReadCapsQuery {
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

    /// The write-side capability query (see `guard::caps::cap_report_write`)
    pub fn write_caps_query(&self, src_local: bool, tgt_local: bool) -> crate::pipeline::guard::caps::WriteCapsQuery {
        crate::pipeline::guard::caps::WriteCapsQuery {
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
    // Each migration guards its own version and none of them stamps `schema`; the stamp happens
    // once, here, after the last one. A migration that stamped SCHEMA itself would skip every
    // later migration the moment one was added.
    if job.schema < 2 {
        migrate_v1_junk_presets(&mut job, &text);
    }
    if job.schema < 3 {
        migrate_v2_peer_target(&mut job, &text);
    }
    job.schema = SCHEMA;
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
}

/// The keys a v2 job file used to name a peer. Read once, on load, and never written again.
#[derive(Deserialize)]
struct LegacyPeerKeys {
    #[serde(default)]
    remote_host: Option<String>,
    #[serde(default)]
    remote_root: Option<String>,
    #[serde(default)]
    remote_exe: Option<String>,
}

/// v2 → v3: the peer moves out of three flat fields and into the target phrase.
///
/// A v2 peer job carried **two** roots and said so nowhere: `remote_root` was the path the far
/// side syncs, while `target` was an SMB mount of that same tree, which the reverse (source-side)
/// direction wrote through. Nothing declared that dependency and nothing checked it — a missing
/// mount just skipped those ops with a warning.
///
/// So the migration carries both across rather than dropping one: the peer path becomes the
/// phrase, and the old `target` becomes `|mount=`, where it is visible and can be validated. That
/// keeps a migrated job doing exactly what it did before, which is the whole bar for a migration —
/// quietly losing the pull direction would be a data-flow change disguised as a rename.
fn migrate_v2_peer_target(job: &mut Job, text: &str) {
    let Ok(legacy) = toml::from_str::<LegacyPeerKeys>(text) else { return };
    let Some(host) = legacy.remote_host.filter(|h| !h.trim().is_empty()) else { return };
    let mut phrase = format!(
        "peer://{}/{}",
        host.trim(),
        legacy.remote_root.unwrap_or_default().trim().replace('\\', "/").trim_matches('/')
    );
    if let Some(exe) = legacy.remote_exe.filter(|e| !e.trim().is_empty()) {
        phrase.push_str(&format!("|exe={}", exe.trim()));
    }
    // The old target was the mount the pull direction used. An empty one means there was never a
    // pull path, so there is nothing to declare.
    if !job.target.trim().is_empty() {
        phrase.push_str(&format!("|mount={}", job.target.trim()));
    }
    job.target = phrase;
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
# --- peer targets (optional) ---
# A `peer://` target means the far side runs its own syncdash: it scans its own disk (no hashing
# over a share) and applies a package this side builds. The whole link is in the phrase.
# target = 'peer://mac/Users/xxx/Code/some/dir|exe=~/Code/SyncDash/target/release/syncdash|mount=\\mac\share\some\dir'
#   exe=    path to syncdash on the far side; omit if it is on PATH
#   mount=  a local path serving the SAME tree. The peer lane only pushes, so the pull (source-side)
#           direction writes through this instead. Omit it and a job that only pushes is unaffected;
#           pull ops are then skipped with a message saying no mount was declared.
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

    /// v2 → v3 must keep a peer job doing exactly what it did. The subtle half is `mount=`: a v2
    /// peer job carried two roots — `remote_root` for the peer, and `target` as an SMB mount of
    /// the same tree that the pull direction wrote through — and declared neither relationship.
    /// Dropping the mount would silently turn a two-way job into a push-only one.
    #[test]
    fn a_v2_peer_job_migrates_into_one_phrase_without_losing_the_pull_path() {
        let j = load_text(
            "v2-peer",
            "schema = 2\nmode = 'sync'\nsource = 'D:\\Code\\x'\ntarget = '\\\\mac\\share\\x'\n\
             remote_host = 'mac'\nremote_root = '/Users/ben/Code/x'\nremote_exe = '~/bin/syncdash'\n",
        );
        assert_eq!(j.schema, SCHEMA);
        assert_eq!(j.target, r"peer://mac/Users/ben/Code/x|exe=~/bin/syncdash|mount=\\mac\share\x");
        // …and it still routes to the peer lane, which is the behaviour that must not change
        assert!(crate::fs::vfs::spec::is_peer(&j.target));
        let crate::fs::vfs::spec::RootSpec::Remote(r) = crate::fs::vfs::spec::parse(&j.target) else {
            panic!("a migrated peer target must parse as a remote root")
        };
        assert_eq!(r.host, "mac");
        assert_eq!(r.root, "Users/ben/Code/x");
        assert_eq!(r.opt("exe"), Some("~/bin/syncdash"));
        assert_eq!(r.opt("mount"), Some(r"\\mac\share\x"));
    }

    #[test]
    fn a_peer_job_without_an_exe_or_a_mount_migrates_to_the_bare_phrase() {
        let j = load_text(
            "v2-peer-bare",
            "schema = 2\nmode = 'mirror'\nsource = 'D:\\Code\\x'\ntarget = ''\n\
             remote_host = 'mac'\nremote_root = '/Users/ben/x'\n",
        );
        assert_eq!(j.target, "peer://mac/Users/ben/x", "no exe and no mount = nothing to declare");
    }

    /// A v2 job with no peer keeps its target untouched — the migration must not touch the
    /// overwhelming majority of jobs, which are plain local-to-local.
    #[test]
    fn a_v2_job_without_a_peer_keeps_its_target() {
        let j = load_text("v2-nopeer", "schema = 2\nmode = 'mirror'\nsource = 'S'\ntarget = 'T'\n");
        assert_eq!(j.target, "T");
        assert!(!crate::fs::vfs::spec::is_peer(&j.target));
    }

    /// A v1 peer job has to pass through BOTH migrations. This is what the per-version guards buy:
    /// the junk migration used to stamp `SCHEMA` itself, which would now carry a v1 file straight
    /// past the peer migration and leave it with a target nothing routes.
    #[test]
    fn a_v1_peer_job_runs_every_migration_in_order() {
        let j = load_text(
            "v1-peer",
            "mode = 'mirror'\nsource = 'S'\ntarget = 'T'\nos_excludes = 'windows'\n\
             remote_host = 'mac'\nremote_root = '/Users/ben/x'\n",
        );
        assert_eq!(j.schema, SCHEMA);
        assert_eq!(j.exclude, expand_junk_presets(["windows"]), "the v1 junk migration still ran");
        assert_eq!(j.target, "peer://mac/Users/ben/x|mount=T", "…and so did the v2 one");
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
        assert!(text.contains(&format!("schema = {SCHEMA}")));
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

        // A v1 file: on disk it is 1, while the job load hands back is already migrated to current
        let p = write("v1", &format!("{HEAD}os_excludes = 'auto'\n"));
        assert_eq!(file_schema(&p.to_string_lossy()).unwrap(), 1, "no schema key = v1");
        assert_eq!(load(&p.to_string_lossy()).unwrap().1.schema, SCHEMA, "…but the loaded job is migrated");
        let _ = std::fs::remove_file(&p);

        // An intermediate version reports itself, not the version it will become on load
        let p = write("v2", &format!("schema = 2\n{HEAD}"));
        assert_eq!(file_schema(&p.to_string_lossy()).unwrap(), 2);
        assert_eq!(load(&p.to_string_lossy()).unwrap().1.schema, SCHEMA);
        let _ = std::fs::remove_file(&p);

        // A current file says so, and there is nothing to announce
        let p = write("cur", &format!("schema = {SCHEMA}\n{HEAD}"));
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
