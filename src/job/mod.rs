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
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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

fn default_true() -> bool {
    true
}

/// Current job-file schema. Bump when a load-time migration is added, and give the migration a
/// `schema < N` guard — the version is what tells "the user deleted this rule" apart from
/// "this file predates the rule", which no amount of inspecting the contents can.
pub const SCHEMA: u32 = 4;
fn default_schema() -> u32 {
    1
}

/// Stable identity of one effective job configuration.
///
/// Hash the migrated, current-schema `Job` value rather than the TOML bytes on disk: formatting,
/// comments, and key order are not configuration changes, while every field the engine can observe
/// is. `job_id` is registry metadata rather than engine policy, so it is cleared before encoding.
/// The domain prefix versions this canonical encoding independently of the job-file schema.
pub fn config_revision(job: &Job) -> Result<String, String> {
    for (field, value) in [
        ("min_free_pct", job.min_free_pct),
        ("max_delete_ratio", job.max_delete_ratio),
    ] {
        if !value.is_finite() {
            return Err(format!(
                "cannot identify this job configuration: {field} must be a finite number"
            ));
        }
    }
    let canonical = Job {
        schema: SCHEMA,
        job_id: String::new(),
        ..job.clone()
    };
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|e| format!("cannot identify this job configuration: {e}"))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"syncdash-job-config-v2\0");
    hasher.update(&bytes);
    Ok(hasher.finalize().to_hex().to_string())
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

    /// Validate every persisted engine setting before the job can be compared or written.
    pub fn validate(&self) -> Result<(), String> {
        validate_choice("mode", &self.mode, &["mirror", "sync", "enrich"])?;
        validate_choice(
            "rigor",
            &self.rigor,
            &[
                "quick", "fast", "balanced", "standard", "paranoid", "custom",
            ],
        )?;
        if let Some(evidence) = self.evidence.as_deref() {
            validate_choice("evidence", evidence, &["none", "sampled", "full"])?;
        }
        validate_choice("symlinks", &self.symlinks, &["exclude", "direct"])?;
        validate_choice(
            "on_conflict",
            &self.on_conflict,
            &["report", "copy", "newer"],
        )?;

        validate_ratio("min_free_pct", self.min_free_pct)?;
        validate_non_negative("max_delete_ratio", self.max_delete_ratio)?;
        if self.max_conflicts < -1 {
            return Err("max_conflicts must be -1 (unlimited) or zero or greater".into());
        }
        if let Some(parallel) = self.parallel {
            if !(1..=16).contains(&parallel) {
                return Err("parallel must be between 1 and 16".into());
            }
        }
        if self.autoscan_interval_secs == Some(0) {
            return Err(
                "autoscan_interval_secs must be at least 1 when AutoScan is enabled".into(),
            );
        }
        if self.autoscan_auto_apply && self.autoscan_interval_secs.is_none() {
            return Err("autoscan_auto_apply requires autoscan_interval_secs".into());
        }

        self.validate_target_rules()?;
        validate_root_relationships(&self.source, &self.targets)
    }

    fn validate_target_rules(&self) -> Result<(), String> {
        if self.targets.is_empty() {
            return Err("a job must contain at least one target root".into());
        }
        if self.targets.len() > 1 {
            if self.mode == "sync" {
                return Err("sync mode does not support multiple targets (N-way merge needs version-vector attribution) — use paired sync jobs instead (hub-and-spoke)".into());
            }
            if self
                .targets
                .iter()
                .any(|t| crate::fs::vfs::spec::is_peer(t))
            {
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
        if self.source.trim().is_empty() {
            return Err("source root cannot be empty".into());
        }
        if self.targets.iter().any(|target| target.trim().is_empty()) {
            return Err("target root cannot be empty".into());
        }
        for (label, s) in std::iter::once(("source", &self.source))
            .chain(self.targets.iter().map(|target| ("target", target)))
        {
            match parse(s) {
                RootSpec::UnknownScheme { scheme, .. } => {
                    return Err(format!(
                        "{label} '{s}': unknown scheme '{scheme}://' — refusing to treat it as a local path (known: {})",
                        KNOWN_SCHEMES.join(", ")
                    ));
                }
                RootSpec::Endpoint(endpoint) if endpoint.host.trim().is_empty() => {
                    return Err(format!("{label} '{s}': endpoint host cannot be empty"));
                }
                RootSpec::Endpoint(endpoint)
                    if endpoint.root.split('/').any(|segment| segment == "..") =>
                {
                    return Err(format!(
                        "{label} '{s}': endpoint root cannot contain a '..' segment"
                    ));
                }
                _ => {}
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

    /// Bind the persisted configuration to exactly one target before entering a phrase-based
    /// compare or apply pipeline.
    pub fn select_target(&self, target_index: usize) -> Result<SingleTargetJob, String> {
        self.validate()?;
        let target = self.targets.get(target_index).cloned().ok_or_else(|| {
            format!(
                "target index {target_index} is out of range ({} total)",
                self.targets.len()
            )
        })?;
        let mut configuration = self.clone();
        configuration.targets = vec![target];
        Ok(SingleTargetJob {
            configuration,
            target_index,
        })
    }
}

/// A validated job configuration normalized to exactly one target, plus its index in the persisted
/// job that selected it.
///
/// Phrase-based execution accepts this type instead of `Job`, so no run can accidentally infer a
/// scalar target from a multi-target configuration. The fields stay private to preserve the
/// validation, bounds, and one-target checks performed by `Job::select_target`.
#[derive(Clone, Debug)]
pub struct SingleTargetJob {
    configuration: Job,
    target_index: usize,
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

    /// The target root as a filesystem path (valid for local roots; VFS roots go through `vfs::open`).
    pub fn target_path(&self) -> &std::path::Path {
        std::path::Path::new(self.target())
    }
}

fn validate_choice(field: &str, value: &str, accepted: &[&str]) -> Result<(), String> {
    if accepted.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{field} must be one of {} (got '{value}')",
            accepted.join(", ")
        ))
    }
}

fn validate_ratio(field: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("{field} must be a finite number"));
    }
    if !(0.0..=1.0).contains(&value) {
        return Err(format!("{field} must be between 0 and 1"));
    }
    Ok(())
}

fn validate_non_negative(field: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("{field} must be a finite number"));
    }
    if value < 0.0 {
        return Err(format!("{field} must be zero or greater"));
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
struct ComparableLocalRoot {
    prefix: String,
    segments: Vec<String>,
}

#[derive(PartialEq, Eq)]
struct ComparableEndpointRoot {
    endpoint: String,
    segments: Vec<String>,
}

fn comparable_endpoint_root(raw: &str) -> Option<ComparableEndpointRoot> {
    use crate::fs::vfs::spec::{default_port, parse, RootSpec};
    let RootSpec::Endpoint(endpoint_spec) = parse(raw) else {
        return None;
    };
    let user = endpoint_spec.user.as_deref().unwrap_or("");
    let endpoint = format!(
        "{}://{}@{}:{}",
        endpoint_spec.scheme,
        user,
        endpoint_spec.host.to_lowercase(),
        endpoint_spec
            .port
            .unwrap_or(default_port(&endpoint_spec.scheme)),
    );
    let case_insensitive = endpoint_spec.scheme == "smb";
    let segments = endpoint_spec
        .root
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(|segment| {
            if case_insensitive {
                segment.to_lowercase()
            } else {
                segment.to_string()
            }
        })
        .collect();
    Some(ComparableEndpointRoot { endpoint, segments })
}

fn comparable_local_root(raw: &str) -> Option<ComparableLocalRoot> {
    use crate::fs::vfs::spec::{parse, RootSpec};
    let RootSpec::Local(_) = parse(raw) else {
        return None;
    };

    let unified = raw.trim().replace('\\', "/");
    let windows =
        cfg!(windows) || unified.starts_with("//") || unified.as_bytes().get(1) == Some(&b':');
    let normalized = if windows {
        unified.to_lowercase()
    } else {
        unified
    };
    let prefix = if normalized.starts_with("//") {
        "//"
    } else if normalized.starts_with('/') {
        "/"
    } else if normalized.as_bytes().get(1) == Some(&b':') {
        &normalized[..2]
    } else {
        ""
    };
    let body = normalized
        .strip_prefix(prefix)
        .unwrap_or(&normalized)
        .trim_start_matches('/');
    let mut segments = Vec::new();
    for segment in body
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
    {
        if segment == ".." {
            if matches!(segments.last().map(String::as_str), Some(last) if last != "..") {
                segments.pop();
            } else if prefix.is_empty() {
                segments.push(segment.to_string());
            }
        } else {
            segments.push(segment.to_string());
        }
    }
    Some(ComparableLocalRoot {
        prefix: prefix.to_string(),
        segments,
    })
}

fn validate_root_relationships(source: &str, targets: &[String]) -> Result<(), String> {
    let source_local = comparable_local_root(source);
    let source_endpoint = comparable_endpoint_root(source);
    let source_identity = root_identity(source);
    let mut seen = Vec::<String>::new();
    for (index, target) in targets.iter().enumerate() {
        let identity = root_identity(target);
        if identity == source_identity {
            return Err("source and target must be different directories".into());
        }
        if seen.iter().any(|existing| existing == &identity) {
            return Err(format!("target {} duplicates an earlier target", index + 1));
        }
        seen.push(identity);

        if let (Some(source_root), Some(target_root)) =
            (source_local.as_ref(), comparable_local_root(target))
        {
            if source_root.prefix == target_root.prefix {
                if target_root.segments.starts_with(&source_root.segments) {
                    return Err("target cannot be nested inside source".into());
                }
                if source_root.segments.starts_with(&target_root.segments) {
                    return Err("source cannot be nested inside target".into());
                }
            }
        }

        if let (Some(source_root), Some(target_root)) =
            (source_endpoint.as_ref(), comparable_endpoint_root(target))
        {
            if source_root.endpoint == target_root.endpoint {
                if target_root.segments.starts_with(&source_root.segments) {
                    return Err("target cannot be nested inside source".into());
                }
                if source_root.segments.starts_with(&target_root.segments) {
                    return Err("source cannot be nested inside target".into());
                }
            }
        }
    }
    Ok(())
}

pub fn validate_root_pair(source: &str, target: &str) -> Result<(), String> {
    validate_root_relationships(source, &[target.to_string()])
}

fn root_identity(raw: &str) -> String {
    match crate::fs::vfs::spec::parse(raw) {
        crate::fs::vfs::spec::RootSpec::Endpoint(endpoint) => {
            format!("endpoint:{}", endpoint.identity())
        }
        crate::fs::vfs::spec::RootSpec::Local(_) => comparable_local_root(raw)
            .map(|root| format!("local:{}:{}", root.prefix, root.segments.join("/")))
            .unwrap_or_else(|| format!("local:{}", raw.trim())),
        crate::fs::vfs::spec::RootSpec::UnknownScheme { raw, .. } => {
            format!("unknown:{raw}")
        }
    }
}

/// The **resolved** rigor level: the preset lays the base, the four detail Options override it.

impl Job {
    /// Resolve a rigor preset plus any explicit detail overrides.
    pub fn rigor_resolved(&self) -> RigorResolved {
        RigorResolved::from_preset(&self.rigor)
            .with_evidence(self.evidence.as_deref())
            .with_cache(self.use_cache)
            .with_escalate(self.escalate)
            .with_verify_writes(self.verify_writes)
    }

    /// The read-side capability query (window already widened to the coarser backend)
    pub fn read_caps_query(
        &self,
        window_ms: i64,
        src_local: bool,
        tgt_local: bool,
    ) -> crate::pipeline::guard::caps::ReadCapsQuery {
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
    pub fn write_caps_query(
        &self,
        src_local: bool,
        tgt_local: bool,
    ) -> crate::pipeline::guard::caps::WriteCapsQuery {
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
            // Root resolution widens this to the coarser backend precision.
            mtime_window_ms: crate::model::plan::MTIME_SLACK_MS,
        }
    }

    pub fn apply_opts(
        &self,
        trash: Option<PathBuf>,
        verbose: bool,
    ) -> crate::pipeline::apply::ApplyOptions {
        crate::pipeline::apply::ApplyOptions {
            dry_run: false,
            trash,
            verbose,
            // Verify-after-write reads the resolved knob (on by default in the balanced/standard/paranoid presets; a detail knob can override)
            verify: self.rigor_resolved().verify_writes,
            versioning: self.versioning,
            fsync: self.fsync,
            filter: Some(crate::pipeline::filter::PathFilter::build_full(
                &self.include,
                &self.exclude,
                &self.deletable,
            )),
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
            format!(
                "job not found: {name_or_path} (looked at {})",
                cand.display()
            ),
        ));
    }
    Ok(cand)
}

/// Resolve a registered job name without accepting a path-shaped alias. Desktop IPC always deals
/// in names returned by `load_all`; direct paths remain a CLI-only convenience through `load`.
fn registered_job_path(name: &str) -> std::io::Result<PathBuf> {
    if name.is_empty()
        || name.trim() != name
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':'])
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid registered job name: {name}"),
        ));
    }
    Ok(crate::foundation::dirs::jobs_dir().join(format!("{name}.toml")))
}

fn existing_registered_job_path(name: &str) -> std::io::Result<PathBuf> {
    let path = registered_job_path(name)?;
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name} (looked at {})", path.display()),
        ));
    }
    Ok(path)
}

/// The schema a job file declares **as it sits on disk**, before `load` migrates it.
///
/// `load` returns the migrated job, so by then the difference has been erased — but a v1 file's junk
/// rules have just been materialized into `exclude`, and anything showing that list has to be able to
/// say those lines are not in the file yet. A file with no `schema` key predates versioning: v1.
pub fn file_schema(name_or_path: &str) -> std::io::Result<u32> {
    let path = resolve_path(name_or_path)?;
    file_schema_at(&path)
}

/// Read the on-disk schema for a job registered in the jobs directory.
pub fn file_schema_named(name: &str) -> std::io::Result<u32> {
    file_schema_at(&existing_registered_job_path(name)?)
}

fn file_schema_at(path: &Path) -> std::io::Result<u32> {
    #[derive(Deserialize)]
    struct OnlySchema {
        #[serde(default = "default_schema")]
        schema: u32,
    }
    let text = std::fs::read_to_string(&path)?;
    let parsed: OnlySchema = toml::from_str(&text).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad job file {}: {e}", path.display()),
        )
    })?;
    Ok(parsed.schema)
}

pub fn load(name_or_path: &str) -> std::io::Result<(String, Job)> {
    let path = PathBuf::from(name_or_path);
    if path.is_file() {
        load_path(&path)
    } else {
        load_named(name_or_path)
    }
}

/// Load exactly one job registered in the jobs directory. Unlike `load`, this never interprets the
/// argument as a path, so an IPC caller cannot substitute an old same-stem file from elsewhere.
pub fn load_named(name: &str) -> std::io::Result<(String, Job)> {
    let dir = crate::foundation::dirs::jobs_dir();
    let _lock = lock_job_mutations(&dir)?;
    let path = registered_job_path_in(&dir, name)?;
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name} (looked at {})", path.display()),
        ));
    }
    load_registered_path(&path)
}

fn load_path(path: &Path) -> std::io::Result<(String, Job)> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = std::fs::read_to_string(&path)?;
    let mut job: Job = toml::from_str(&text).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad job file {}: {e}", path.display()),
        )
    })?;
    if job.schema > SCHEMA {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bad job file {}: schema v{} is newer than supported schema v{}",
                path.display(),
                job.schema,
                SCHEMA
            ),
        ));
    }
    // Each migration guards its own version and none of them stamps `schema`; the stamp happens
    // once, here, after the last one. A migration that stamped SCHEMA itself would skip every
    // later migration the moment one was added.
    if job.schema < 2 {
        migrate_v1_junk_presets(&mut job, &text);
    }
    if job.schema < 3 {
        migrate_v2_peer_target(&mut job, &text);
    }
    if job.schema < 4 {
        migrate_v3_current_schema(&mut job, &text).map_err(|reason| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad job file {}: {reason}", path.display()),
            )
        })?;
    }
    if job.targets.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bad job file {}: current job schema requires at least one target",
                path.display()
            ),
        ));
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
    if !job.targets.is_empty() {
        return;
    }
    let Ok(legacy) = toml::from_str::<LegacyPeerKeys>(text) else {
        return;
    };
    let Some(host) = legacy.remote_host.filter(|h| !h.trim().is_empty()) else {
        return;
    };
    let mut phrase = format!(
        "peer://{}/{}",
        host.trim(),
        legacy
            .remote_root
            .unwrap_or_default()
            .trim()
            .replace('\\', "/")
            .trim_matches('/')
    );
    if let Some(exe) = legacy.remote_exe.filter(|e| !e.trim().is_empty()) {
        phrase.push_str(&format!("|exe={}", exe.trim()));
    }
    // The old target was the mount the pull direction used. An empty one means there was never a
    // pull path, so there is nothing to declare.
    let Ok(mut legacy) = toml::from_str::<LegacyV3Fields>(text) else {
        return;
    };
    let legacy_target = legacy.target.take().unwrap_or_default();
    if !legacy_target.trim().is_empty() {
        phrase.push_str(&format!("|mount={}", legacy_target.trim()));
    }
    job.targets = vec![phrase];
}

#[derive(Deserialize)]
struct LegacyV3Fields {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    watch_interval_secs: Option<u64>,
    #[serde(default)]
    watch_auto_apply: bool,
    #[serde(default)]
    no_hash: bool,
}

/// v3 → v4: remove the three settings that duplicated current runtime policy.
///
/// The target precedence and `no_hash` override are reproduced here once so legacy jobs retain
/// their effective roots and evidence behavior. After this boundary the current `Job` type carries
/// only canonical target, AutoScan, and evidence fields.
fn migrate_v3_current_schema(job: &mut Job, text: &str) -> Result<(), String> {
    let legacy: LegacyV3Fields =
        toml::from_str(text).map_err(|error| format!("cannot read legacy settings: {error}"))?;
    if job.targets.is_empty() {
        let target = legacy
            .target
            .ok_or_else(|| "legacy job has neither `target` nor `targets`".to_string())?;
        job.targets.push(target);
    }
    job.autoscan_interval_secs = legacy.watch_interval_secs;
    job.autoscan_auto_apply = legacy.watch_auto_apply;
    if legacy.no_hash {
        let verify_writes = job.rigor_resolved().verify_writes;
        job.rigor = "custom".into();
        job.evidence = Some("none".into());
        job.use_cache = Some(false);
        job.escalate = Some(false);
        job.verify_writes = Some(verify_writes);
    }
    Ok(())
}

pub fn load_all() -> std::io::Result<Vec<(String, Job)>> {
    let dir = crate::foundation::dirs::jobs_dir();
    let _lock = lock_job_mutations(&dir)?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            out.push(load_registered_path(&path)?);
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));

    let mut identities = std::collections::BTreeMap::new();
    for (name, job) in &out {
        if let Some(previous) = identities.insert(job.job_id.as_str(), name.as_str()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "jobs '{previous}' and '{name}' carry the same job_id '{}' — each registered job must have a unique identity",
                    job.job_id
                ),
            ));
        }
    }
    Ok(out)
}

/// Resolve a registered job by its stable identity rather than its mutable filename. Result and
/// AutoScan ownership use this after a rename so an old display name can never make evidence attach
/// to a newly-created job that happens to reuse that name.
pub fn load_by_id(job_id: &str) -> std::io::Result<(String, Job)> {
    validate_job_id(job_id)?;
    load_all()?
        .into_iter()
        .find(|(_, job)| job.job_id == job_id)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("job identity not found: {job_id}"),
            )
        })
}

fn validate_job_id(job_id: &str) -> std::io::Result<()> {
    if job_id.len() == 32
        && job_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "job_id must be 32 lowercase hexadecimal characters",
        ))
    }
}

fn new_job_id() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| std::io::Error::other(format!("cannot create a job identity: {error}")))?;
    let mut id = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        id.push(HEX[(byte >> 4) as usize] as char);
        id.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(id)
}

fn staged_text(path: &Path, text: &str) -> std::io::Result<crate::fs::staged::Staged> {
    let mut staged = crate::fs::staged::Staged::create(path)?;
    staged.write_all_from(&mut text.as_bytes())?;
    staged.seal(true)?;
    Ok(staged)
}

/// Materialize identity metadata without serializing the whole job. Prepending one top-level key
/// preserves comments and, critically, leaves an old `schema` visible to the editor until the user
/// explicitly saves the migrated configuration.
fn load_registered_path(path: &Path) -> std::io::Result<(String, Job)> {
    let (name, mut job) = load_path(path)?;
    if job.job_id.is_empty() {
        let text = std::fs::read_to_string(path)?;
        let parsed: toml::Value = toml::from_str(&text).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad job file {}: {error}", path.display()),
            )
        })?;
        if parsed.get("job_id").is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad job file {}: job_id cannot be empty", path.display()),
            ));
        }
        job.job_id = new_job_id()?;
        let with_identity = format!("job_id = \"{}\"\n{text}", job.job_id);
        staged_text(path, &with_identity)?.commit()?;
    } else {
        validate_job_id(&job.job_id).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("bad job file {}: {error}", path.display()),
            )
        })?;
    }
    Ok((name, job))
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub enum JobMutationEffect {
    Created,
    Updated,
    Renamed,
    NoOp,
    Deleted,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub enum JobRootField {
    Source,
    Target,
}

#[derive(Debug)]
pub struct SavedJob {
    pub name: String,
    pub path: PathBuf,
    pub job_id: String,
    pub config_revision: String,
    pub effect: JobMutationEffect,
    pub previous_name: Option<String>,
}

#[derive(Debug)]
pub struct JobRootMutation {
    pub mutation: SavedJob,
    pub source: String,
    pub targets: Vec<String>,
}

#[derive(Debug)]
pub struct DeletedJob {
    pub name: String,
    pub job_id: String,
    pub config_revision: String,
    pub effect: JobMutationEffect,
}

struct JobMutationLock {
    _file: std::fs::File,
}

fn lock_job_mutations(dir: &Path) -> std::io::Result<JobMutationLock> {
    std::fs::create_dir_all(dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(dir.join(".syncdash-jobs.lock"))?;
    file.lock()?;
    Ok(JobMutationLock { _file: file })
}

fn registered_job_path_in(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    registered_job_path(name).map(|path| {
        path.file_name()
            .map(|file_name| dir.join(file_name))
            .unwrap_or(path)
    })
}

fn invalid_job(reason: String) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("invalid job: {reason}"),
    )
}

fn current_revision_at(path: &Path) -> std::io::Result<String> {
    let (_, current) = load_path(path)?;
    config_revision(&current).map_err(invalid_job)
}

fn require_revision(path: &Path, name: &str, expected: &str) -> std::io::Result<()> {
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name}"),
        ));
    }
    let current = current_revision_at(path)?;
    if current != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' changed on disk (expected revision {expected}, found {current}) — reload before saving"
            ),
        ));
    }
    Ok(())
}

fn load_expected_job(
    path: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
) -> std::io::Result<Job> {
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name}"),
        ));
    }
    let (_, current) = load_path(path)?;
    validate_job_id(&current.job_id).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("bad job file {}: {error}", path.display()),
        )
    })?;
    if current.job_id != expected_job_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' was replaced since this editor loaded it (expected job_id '{expected_job_id}', found '{}') — reload before saving",
                current.job_id
            ),
        ));
    }
    let current_revision = config_revision(&current).map_err(invalid_job)?;
    if current_revision != expected_config_revision {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' changed on disk (expected revision {expected_config_revision}, found {current_revision}) — reload before saving"
            ),
        ));
    }
    Ok(current)
}

fn staged_job(path: &Path, job: &Job) -> std::io::Result<crate::fs::staged::Staged> {
    let text = toml::to_string_pretty(job).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("toml serialize: {e}"),
        )
    })?;
    staged_text(path, &text)
}

fn rename_without_overwrite(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::hard_link(source, destination)?;
    if let Err(error) = std::fs::remove_file(source) {
        if let Err(rollback) = std::fs::remove_file(destination) {
            return Err(std::io::Error::new(
                error.kind(),
                format!(
                    "cannot remove the old job name: {error}; removing the collision-safe link also failed: {rollback}"
                ),
            ));
        }
        return Err(error);
    }
    Ok(())
}

fn require_target(job: &Job, target_index: usize) -> std::io::Result<()> {
    if target_index >= job.targets.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "target_index {target_index} is out of range for {} target(s)",
                job.targets.len()
            ),
        ));
    }
    Ok(())
}

fn target_mut(job: &mut Job, target_index: usize) -> &mut String {
    &mut job.targets[target_index]
}

fn mutate_job_roots_in<F>(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
    mutate: F,
) -> std::io::Result<JobRootMutation>
where
    F: FnOnce(&mut Job) -> std::io::Result<()>,
{
    validate_job_id(expected_job_id)?;
    let _lock = lock_job_mutations(dir)?;
    let path = registered_job_path_in(dir, name)?;
    let mut current = load_expected_job(&path, name, expected_job_id, expected_config_revision)?;
    require_target(&current, target_index)?;
    mutate(&mut current)?;
    current.schema = SCHEMA;
    current.validate().map_err(invalid_job)?;
    let config_revision = config_revision(&current).map_err(invalid_job)?;
    let effect = if config_revision == expected_config_revision && file_schema_at(&path)? == SCHEMA
    {
        JobMutationEffect::NoOp
    } else {
        let staged = staged_job(&path, &current)?;
        load_expected_job(&path, name, expected_job_id, expected_config_revision)?;
        staged.commit()?;
        JobMutationEffect::Updated
    };
    Ok(JobRootMutation {
        mutation: SavedJob {
            name: name.to_string(),
            path,
            job_id: current.job_id,
            config_revision,
            effect,
            previous_name: None,
        },
        source: current.source,
        targets: current.targets,
    })
}

pub fn update_job_root(
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
    field: JobRootField,
    value: &str,
) -> std::io::Result<JobRootMutation> {
    update_job_root_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
        field,
        value,
    )
}

fn update_job_root_in(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
    field: JobRootField,
    value: &str,
) -> std::io::Result<JobRootMutation> {
    if value.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "root value cannot be empty",
        ));
    }
    mutate_job_roots_in(
        dir,
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
        |current| {
            match field {
                JobRootField::Source => current.source = value.to_string(),
                JobRootField::Target => {
                    *target_mut(current, target_index) = value.to_string();
                }
            }
            Ok(())
        },
    )
}

pub fn swap_job_roots(
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
) -> std::io::Result<JobRootMutation> {
    swap_job_roots_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
    )
}

fn swap_job_roots_in(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
) -> std::io::Result<JobRootMutation> {
    mutate_job_roots_in(
        dir,
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
        |current| {
            let previous_source = std::mem::take(&mut current.source);
            let previous_target =
                std::mem::replace(target_mut(current, target_index), previous_source);
            current.source = previous_target;
            Ok(())
        },
    )
}

/// Create, update, or rename one registered job without overwriting an unseen revision.
pub fn save_job(
    name: &str,
    job: &Job,
    original_name: Option<&str>,
    expected_revision: Option<&str>,
) -> std::io::Result<SavedJob> {
    job.validate().map_err(invalid_job)?;
    let job = Job {
        schema: SCHEMA,
        ..job.clone()
    };
    let config_revision = config_revision(&job).map_err(invalid_job)?;
    let dir = crate::foundation::dirs::jobs_dir();
    save_job_in(
        &dir,
        name,
        &job,
        original_name,
        expected_revision,
        config_revision,
    )
}

fn save_job_in(
    dir: &Path,
    name: &str,
    job: &Job,
    original_name: Option<&str>,
    expected_revision: Option<&str>,
    config_revision: String,
) -> std::io::Result<SavedJob> {
    let _lock = lock_job_mutations(dir)?;
    let destination = registered_job_path_in(dir, name)?;
    let mut persisted = job.clone();
    let mut effect = JobMutationEffect::Created;
    let mut previous_name = None;
    match (original_name, expected_revision) {
        (None, None) => {
            if !persisted.job_id.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a new job must not supply job_id; the registry assigns a fresh identity",
                ));
            }
            if destination.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("job '{name}' already exists — reload it before saving"),
                ));
            }
            persisted.job_id = new_job_id()?;
            let staged = staged_job(&destination, &persisted)?;
            if destination.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("job '{name}' was created while this save was being prepared"),
                ));
            }
            staged.commit()?;
        }
        (Some(original_name), Some(expected_revision)) => {
            let original = registered_job_path_in(dir, original_name)?;
            require_revision(&original, original_name, expected_revision)?;
            let (_, current) = load_registered_path(&original)?;
            if persisted.job_id != current.job_id {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "job '{original_name}' was replaced since this editor loaded it (expected job_id '{}', found '{}') — reload before saving",
                        persisted.job_id, current.job_id
                    ),
                ));
            }
            if original == destination {
                if config_revision == expected_revision && file_schema_at(&original)? == SCHEMA {
                    return Ok(SavedJob {
                        name: name.to_string(),
                        path: destination,
                        job_id: persisted.job_id,
                        config_revision,
                        effect: JobMutationEffect::NoOp,
                        previous_name: None,
                    });
                }
                effect = JobMutationEffect::Updated;
                let staged = staged_job(&destination, &persisted)?;
                require_revision(&original, original_name, expected_revision)?;
                staged.commit()?;
            } else {
                effect = JobMutationEffect::Renamed;
                previous_name = Some(original_name.to_string());
                if destination.exists() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("cannot rename job '{original_name}' to '{name}': destination already exists"),
                    ));
                }
                let staged = staged_job(&destination, &persisted)?;
                require_revision(&original, original_name, expected_revision)?;
                if destination.exists() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("job '{name}' was created while this rename was being prepared"),
                    ));
                }
                rename_without_overwrite(&original, &destination)?;
                if let Err(error) = staged.commit() {
                    if let Err(rollback) = rename_without_overwrite(&destination, &original) {
                        return Err(std::io::Error::new(
                            error.kind(),
                            format!("cannot save renamed job: {error}; restoring the original name also failed: {rollback}"),
                        ));
                    }
                    return Err(error);
                }
            }
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "original_name and expected_revision must be supplied together for an update",
            ));
        }
    }
    Ok(SavedJob {
        name: name.to_string(),
        path: destination,
        job_id: persisted.job_id,
        config_revision,
        effect,
        previous_name,
    })
}

pub fn delete_job(
    name: &str,
    expected_job_id: &str,
    expected_revision: &str,
) -> std::io::Result<DeletedJob> {
    delete_job_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        expected_job_id,
        expected_revision,
    )
}

fn delete_job_in(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_revision: &str,
) -> std::io::Result<DeletedJob> {
    let _lock = lock_job_mutations(dir)?;
    let path = registered_job_path_in(dir, name)?;
    require_revision(&path, name, expected_revision)?;
    let (_, current) = load_registered_path(&path)?;
    if current.job_id != expected_job_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' was replaced since this editor loaded it (expected job_id '{expected_job_id}', found '{}') — reload before deleting",
                current.job_id
            ),
        ));
    }
    std::fs::remove_file(path)?;
    Ok(DeletedJob {
        name: name.to_string(),
        job_id: current.job_id,
        config_revision: expected_revision.to_string(),
        effect: JobMutationEffect::Deleted,
    })
}

pub const SAMPLE: &str = r#"# <name>.toml in the jobs directory — one file, one job
schema = 4                              # job-file schema; older scalar-target files migrate on load
# job_id is assigned by the registry on first load/save; do not copy it to another job
mode = "mirror"                         # mirror | sync | enrich
source = 'D:\some\dir'                  # a Windows path; on mac/Linux e.g. '/Users/me/Code'
targets = ['\\host\share\dir']          # one or more roots: local paths, smb:// sftp:// ftp:// ftps:// peer://
# archive = '…/syncdash/archive/<name>.jsonl'   # sync mode only; sits beside this jobs/ directory.
#                                       # Without it deletes and moves are not attributed — `syncdash gen-jobs` writes the path for you
# include = ['*']                       # FFS filter-syntax allowlist (empty = everything)
# exclude = ['*/big_temp/', '*/*.log']  # FFS syntax. The ONLY exclude policy besides this tool's own metadata —
#                                       # junk presets (Windows/macOS/Linux/Developer/IDE/Office/sync tools) write
#                                       # their patterns straight into this list, so it always reads as what runs.
#                                       # `syncdash junk` prints the presets; `syncdash scan --junk <ids>` applies them ad hoc
# rigor = "standard"                    # shortcut preset: quick | fast | balanced | standard | paranoid | custom
# --- rigor detail knobs (a value here overrides the preset's axis; the UI writes them all explicitly) ---
# evidence = "sampled"                  # content evidence: none (0 reads) | sampled (256KB each at head/middle/tail) | full (whole file)
# use_cache = false                     # trust the (path,size,mtime) cache? true in fast/balanced; false from standard up = a real read every round
# escalate = true                       # disagreement escalation: digests equal but mtime differs >2s -> re-verify both sides in full
# verify_writes = true                  # verify after write: hash of the copy stream vs a re-read from disk
# case_sensitive = false                # case-insensitive by default (the NTFS/APFS default)
# symlinks = "exclude"                  # exclude | direct (sync the link itself)
# versioning = true                     # deleted/overwritten files go into each root's .version_syncDash/
#                                       # (browse and recover with syncdash versions / restore; the local trash by default)
#
# --- safety gates ---
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
# --- AutoScan ---
# autoscan_interval_secs = 30           # maximum full verification interval; local macOS roots also react to FSEvents
# autoscan_auto_apply = false           # run an authorized result automatically; notify/review by default
#
# --- peer targets (optional) ---
# A `peer://` target means the far side runs its own syncdash: it scans its own disk (no hashing
# over a share) and applies a package this side builds. The whole link is in the phrase.
# targets = ['peer://mac/Users/xxx/Code/some/dir|exe=~/Code/SyncDash/target/release/syncdash|mount=\\mac\share\some\dir']
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
        let p = std::env::temp_dir().join(format!(
            "syncdash-job-{tag}-{}.toml",
            crate::foundation::time::now_ms()
        ));
        std::fs::write(&p, text).unwrap();
        let (_, j) = load(&p.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&p);
        j
    }

    const HEAD: &str = "mode = 'mirror'\nsource = 'S'\ntarget = 'T'\n";

    #[test]
    fn v1_migration_is_ordered_idempotent_and_preserves_user_rules() {
        // os_excludes = "auto" + dev_excludes = true, the shape `gen-jobs` wrote for every cs-* job
        let j = load_text(
            "auto-dev",
            &format!("{HEAD}os_excludes = 'auto'\ndev_excludes = true\n"),
        );
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
        assert!(
            PathFilter::build(&[], &j.exclude).pass_file("a/Thumbs.db"),
            "'mac' must not drag Windows in"
        );

        let j = load_text(
            "mixed",
            &format!("{HEAD}os_excludes = 'windows'\nexclude = ['*/big_temp/', '*\\Thumbs.db', '!*/keep.log']\n"),
        );
        assert_eq!(
            j.exclude
                .iter()
                .filter(|e| e.to_lowercase().contains("thumbs.db"))
                .count(),
            1
        );
        // Their own entries survive, in their own order, after the preset block
        assert!(j.exclude.contains(&"*/big_temp/".to_string()));
        assert!(j.exclude.contains(&"!*/keep.log".to_string()));
        let pf = PathFilter::build(&[], &j.exclude);
        assert!(!pf.pass_file("x/big_temp/f"));
        assert!(
            pf.pass_file("x/keep.log"),
            "the ! exception must survive the migration"
        );

        let j = load_text(
            "v2",
            &format!("schema = 2\n{HEAD}exclude = ['*/only_this/']\n"),
        );
        assert_eq!(
            j.exclude,
            vec!["*/only_this/".to_string()],
            "v2 files are taken at their word"
        );
        let j = load_text("v2-empty", &format!("schema = 2\n{HEAD}"));
        assert!(
            j.exclude.is_empty(),
            "an empty exclude in a v2 file means empty"
        );

        let j = load_text(
            "v1-peer",
            "mode = 'mirror'\nsource = 'S'\ntarget = 'T'\nos_excludes = 'windows'\n\
             remote_host = 'mac'\nremote_root = '/Users/ben/x'\n",
        );
        assert_eq!(j.schema, SCHEMA);
        assert_eq!(j.exclude, expand_junk_presets(["windows"]));
        assert_eq!(j.targets, vec!["peer://mac/Users/ben/x|mount=T"]);
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
        assert_eq!(
            j.targets,
            vec![r"peer://mac/Users/ben/Code/x|exe=~/bin/syncdash|mount=\\mac\share\x".to_string()]
        );
        // …and it still routes to the peer lane, which is the behaviour that must not change
        assert!(crate::fs::vfs::spec::is_peer(&j.targets[0]));
        let crate::fs::vfs::spec::RootSpec::Endpoint(r) =
            crate::fs::vfs::spec::parse(&j.targets[0])
        else {
            panic!("a migrated peer target must parse as an endpoint root")
        };
        assert_eq!(r.host, "mac");
        assert_eq!(r.root, "Users/ben/Code/x");
        assert_eq!(r.opt("exe"), Some("~/bin/syncdash"));
        assert_eq!(r.opt("mount"), Some(r"\\mac\share\x"));

        let bare = load_text(
            "v2-peer-bare",
            "schema = 2\nmode = 'mirror'\nsource = 'D:\\Code\\x'\ntarget = ''\n\
             remote_host = 'mac'\nremote_root = '/Users/ben/x'\n",
        );
        assert_eq!(
            bare.targets,
            vec!["peer://mac/Users/ben/x".to_string()],
            "no exe and no mount = nothing to declare"
        );

        let local = load_text(
            "v2-nopeer",
            "schema = 2\nmode = 'mirror'\nsource = 'S'\ntarget = 'T'\n",
        );
        assert_eq!(local.targets, vec!["T"]);
        assert!(!crate::fs::vfs::spec::is_peer(&local.targets[0]));
    }

    #[test]
    fn v3_storage_and_runtime_policy_migrate_to_current_fields() {
        let scalar = load_text(
            "v3-scalar-target",
            "schema = 3\nmode = 'mirror'\nsource = 'S'\ntarget = 'T'\n",
        );
        assert_eq!(scalar.targets, vec!["T"]);

        let list = load_text(
            "v3-list-targets",
            "schema = 3\nmode = 'mirror'\nsource = 'S'\ntarget = 'obsolete'\n\
             targets = ['A', 'B']\n",
        );
        assert_eq!(list.targets, vec!["A", "B"]);

        let serialized = toml::to_string_pretty(&list).unwrap();
        assert!(serialized.contains("targets = ["));
        assert!(!serialized.lines().any(|line| line.starts_with("target = ")));

        let migrated = load_text(
            "v3-canonical-policy",
            "schema = 3\nmode = 'mirror'\nsource = 'S'\ntarget = 'T'\n\
             rigor = 'paranoid'\nno_hash = true\nwatch_interval_secs = 45\nwatch_auto_apply = true\n",
        );
        assert_eq!(migrated.autoscan_interval_secs, Some(45));
        assert!(migrated.autoscan_auto_apply);
        assert_eq!(migrated.rigor, "custom");
        assert_eq!(migrated.evidence.as_deref(), Some("none"));
        assert_eq!(migrated.use_cache, Some(false));
        assert_eq!(migrated.escalate, Some(false));
        assert_eq!(migrated.verify_writes, Some(true));
        let resolved = migrated.rigor_resolved();
        assert!(!resolved.hash);
        assert!(resolved.verify_writes);

        let serialized = toml::to_string_pretty(&migrated).unwrap();
        for retired in ["no_hash", "watch_interval_secs", "watch_auto_apply"] {
            assert!(!serialized.lines().any(|line| line.starts_with(retired)));
        }
    }

    #[test]
    fn unsupported_current_and_future_shapes_are_refused() {
        let path = std::env::temp_dir().join(format!(
            "syncdash-job-v4-missing-targets-{}.toml",
            crate::foundation::time::now_ms()
        ));
        std::fs::write(&path, "schema = 4\nmode = 'mirror'\nsource = 'S'\n").unwrap();
        let error = load(&path.to_string_lossy()).unwrap_err();
        let _ = std::fs::remove_file(path);
        assert!(error.to_string().contains("requires at least one target"));

        let path = std::env::temp_dir().join(format!(
            "syncdash-job-future-schema-{}.toml",
            crate::foundation::time::now_ms()
        ));
        std::fs::write(
            &path,
            "schema = 5\nmode = 'mirror'\nsource = 'S'\ntargets = ['T']\n",
        )
        .unwrap();
        let error = load(&path.to_string_lossy()).unwrap_err();
        let _ = std::fs::remove_file(path);
        assert!(error.to_string().contains("schema v5 is newer"));
    }

    /// A Job built in memory and written straight to TOML — which is what `gen-jobs` does, bypassing
    /// `save_job` — must come back exactly as written. It did not: `Job::default()` stamped the *legacy*
    /// schema, so every generated job was re-migrated on load and quietly grew preset patterns its author
    /// never chose. Exclusions appearing on their own is the failure mode this whole design is against.
    #[test]
    fn a_job_written_straight_to_toml_round_trips_untouched() {
        assert_eq!(
            Job::default().schema,
            SCHEMA,
            "a Job built by current code is current-shape"
        );
        let j = Job {
            source: "S".into(),
            targets: vec!["T".into()],
            exclude: vec!["*/only_mine/".into()],
            ..Default::default()
        };
        let p = std::env::temp_dir().join(format!(
            "syncdash-job-rt-{}.toml",
            crate::foundation::time::now_ms()
        ));
        std::fs::write(&p, toml::to_string_pretty(&j).unwrap()).unwrap();
        let (_, back) = load(&p.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&p);
        assert_eq!(
            back.exclude, j.exclude,
            "no rule may appear that the author did not write"
        );
        assert_eq!(back.schema, SCHEMA);
    }

    /// `save_job` stamps the current schema itself. If it trusted a caller-supplied version, a frontend
    /// that omitted the field would round-trip a v2 job back to v1 and the next load would re-add presets.
    #[test]
    fn saving_stamps_the_current_schema() {
        let stale = Job {
            schema: 1,
            exclude: vec!["*/mine/".into()],
            ..Default::default()
        };
        let text = toml::to_string_pretty(&Job {
            schema: SCHEMA,
            ..stale.clone()
        })
        .unwrap();
        assert!(text.contains(&format!("schema = {SCHEMA}")));
        // …and the migration would have fired on the stale one, which is exactly what stamping prevents
        let j = load_text(
            "stale",
            &format!("schema = 1\n{HEAD}exclude = ['*/mine/']\n"),
        );
        assert!(
            j.exclude.len() > 1,
            "premise: schema 1 does trigger the migration"
        );
    }

    /// `file_schema` reports the file, not the loaded job — that difference is the whole point of it.
    /// The editor uses it to say "these exclude lines came from the migration and are not in the file
    /// yet"; if it reported the migrated value it would always say nothing had happened.
    #[test]
    fn file_schema_reports_the_file_not_the_migrated_job() {
        let write = |tag: &str, text: &str| {
            let p = std::env::temp_dir().join(format!(
                "syncdash-fs-{tag}-{}.toml",
                crate::foundation::time::now_ms()
            ));
            std::fs::write(&p, text).unwrap();
            p
        };

        // A v1 file: on disk it is 1, while the job load hands back is already migrated to current
        let p = write("v1", &format!("{HEAD}os_excludes = 'auto'\n"));
        assert_eq!(
            file_schema(&p.to_string_lossy()).unwrap(),
            1,
            "no schema key = v1"
        );
        assert_eq!(
            load(&p.to_string_lossy()).unwrap().1.schema,
            SCHEMA,
            "…but the loaded job is migrated"
        );
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

        assert!(
            file_schema("no-such-job-exists-here").is_err(),
            "a missing file is an error, not a version"
        );
    }

    #[test]
    fn assigning_registered_identity_preserves_legacy_schema_and_comments() {
        let path = std::env::temp_dir().join(format!(
            "syncdash-job-id-migration-{}-{}.toml",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        let text = format!("# keep this explanation\n{HEAD}os_excludes = 'off'\n");
        std::fs::write(&path, &text).unwrap();

        let (_, first) = load_registered_path(&path).unwrap();
        let (_, second) = load_registered_path(&path).unwrap();
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first.job_id, second.job_id);
        validate_job_id(&first.job_id).unwrap();
        assert_eq!(file_schema_at(&path).unwrap(), 1);
        assert!(persisted.contains("# keep this explanation"));
        assert_eq!(persisted.matches("job_id =").count(), 1);

        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;

    #[test]
    fn registered_job_names_cannot_be_reinterpreted_as_paths() {
        for invalid in [
            "../photos",
            "folder/photos",
            r"folder\photos",
            "/tmp/photos",
            "C:photos",
        ] {
            let error = registered_job_path(invalid).unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidInput,
                "{invalid}: {error}"
            );
        }
        let path = registered_job_path("photos.archive").unwrap();
        assert_eq!(path.file_name().unwrap(), "photos.archive.toml");
        assert_eq!(path.parent().unwrap(), crate::foundation::dirs::jobs_dir());
    }

    #[test]
    fn revision_is_stable_for_the_effective_job_and_changes_with_configuration() {
        let original = Job {
            schema: 1,
            source: "source".into(),
            targets: vec!["target".into()],
            exclude: vec!["*.tmp".into()],
            ..Default::default()
        };
        let current_schema = Job {
            schema: SCHEMA,
            ..original.clone()
        };

        let revision = config_revision(&original).unwrap();
        assert_eq!(revision.len(), 64);
        assert_eq!(revision, config_revision(&current_schema).unwrap());

        let identified = Job {
            job_id: "0123456789abcdef0123456789abcdef".into(),
            ..current_schema.clone()
        };
        assert_eq!(
            revision,
            config_revision(&identified).unwrap(),
            "registry identity is not an engine configuration change"
        );

        let changed = Job {
            exclude: vec!["*.tmp".into(), "*.bak".into()],
            ..current_schema
        };
        assert_ne!(revision, config_revision(&changed).unwrap());
    }

    #[test]
    fn revision_rejects_non_finite_configuration_numbers() {
        let job = Job {
            min_free_pct: f64::NAN,
            ..Default::default()
        };
        let error = config_revision(&job).unwrap_err();
        assert!(error.contains("min_free_pct"), "{error}");
        assert!(error.contains("finite"), "{error}");
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn valid_job() -> Job {
        Job {
            source: "/data/source".into(),
            targets: vec!["/data/target".into()],
            ..Default::default()
        }
    }

    #[test]
    fn persisted_choices_are_closed_vocabularies() {
        let cases = [
            ("mode", "unknown"),
            ("rigor", "typo"),
            ("evidence", "sometimes"),
            ("symlinks", "follow"),
            ("on_conflict", "overwrite"),
        ];
        for (field, value) in cases {
            let mut job = valid_job();
            match field {
                "mode" => job.mode = value.into(),
                "rigor" => job.rigor = value.into(),
                "evidence" => job.evidence = Some(value.into()),
                "symlinks" => job.symlinks = value.into(),
                "on_conflict" => job.on_conflict = value.into(),
                _ => unreachable!(),
            }
            let error = job.validate().unwrap_err();
            assert!(error.contains(field), "{field}: {error}");
        }
    }

    #[test]
    fn numeric_settings_refuse_non_finite_and_out_of_range_values() {
        for (field, value) in [
            ("min_free_pct", f64::NAN),
            ("min_free_pct", -0.1),
            ("min_free_pct", 1.1),
            ("max_delete_ratio", f64::INFINITY),
            ("max_delete_ratio", -0.1),
        ] {
            let mut job = valid_job();
            if field == "min_free_pct" {
                job.min_free_pct = value;
            } else {
                job.max_delete_ratio = value;
            }
            let error = job.validate().unwrap_err();
            assert!(error.contains(field), "{field}: {error}");
        }

        let mut job = valid_job();
        job.max_delete_ratio = 1.1;
        assert!(
            job.validate().is_ok(),
            "a ratio >= 1 disables the deletion gate"
        );
        let mut job = valid_job();
        job.parallel = Some(17);
        assert!(job.validate().unwrap_err().contains("parallel"));
        let mut job = valid_job();
        job.max_conflicts = -2;
        assert!(job.validate().unwrap_err().contains("max_conflicts"));
        let mut job = valid_job();
        job.autoscan_interval_secs = Some(0);
        assert!(job
            .validate()
            .unwrap_err()
            .contains("autoscan_interval_secs"));
        let mut job = valid_job();
        job.autoscan_auto_apply = true;
        assert!(job.validate().unwrap_err().contains("autoscan_auto_apply"));
    }

    #[test]
    fn roots_must_be_present_distinct_and_not_nested() {
        let mut job = valid_job();
        job.targets.clear();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("at least one target root"));

        let mut job = valid_job();
        job.source.clear();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("source root cannot be empty"));

        let mut job = valid_job();
        job.targets[0] = "/data/source".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("different directories"));

        let mut job = valid_job();
        job.targets[0] = "/data/source/child".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("target cannot be nested"));

        let mut job = valid_job();
        job.source = r"C:\Data\Source".into();
        job.targets[0] = r"c:/data/source/child".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("target cannot be nested"));

        let mut job = valid_job();
        job.targets = vec!["/data/a".into(), "/data/a".into()];
        assert!(job.validate().unwrap_err().contains("duplicates"));

        let mut job = valid_job();
        job.source = "sftp://user@host/data/source".into();
        job.targets[0] = "sftp://user@HOST:22/data/source/child".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("target cannot be nested"));

        let mut job = valid_job();
        job.source = "sftp://host/data".into();
        job.targets[0] = "sftp://host/data/../escape".into();
        assert!(job
            .validate()
            .unwrap_err()
            .contains("cannot contain a '..'"));
    }

    #[test]
    fn single_target_job_binds_one_validated_target_index() {
        let mut job = valid_job();
        job.mode = "mirror".into();
        job.targets.push("/data/backup".into());
        let selected = job.select_target(1).unwrap();
        assert_eq!(selected.target_index(), 1);
        assert_eq!(selected.target(), "/data/backup");
        assert_eq!(selected.configuration().targets, vec!["/data/backup"]);

        let error = job.select_target(2).unwrap_err();
        assert!(error.contains("out of range"));
    }

    #[test]
    fn persistence_uses_atomic_create_and_revision_checked_update_rename_delete() {
        let dir = std::env::temp_dir().join(format!(
            "syncdash-job-cas-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let first = valid_job();
        let first_revision = config_revision(&first).unwrap();
        let created =
            save_job_in(&dir, "photos", &first, None, None, first_revision.clone()).unwrap();
        assert_eq!(created.config_revision, first_revision);
        assert_eq!(created.effect, JobMutationEffect::Created);
        validate_job_id(&created.job_id).unwrap();

        let identified_first = Job {
            job_id: created.job_id.clone(),
            ..first.clone()
        };
        let no_op = save_job_in(
            &dir,
            "photos",
            &identified_first,
            Some("photos"),
            Some(&first_revision),
            first_revision.clone(),
        )
        .unwrap();
        assert_eq!(no_op.effect, JobMutationEffect::NoOp);

        let mut second = identified_first;
        second.exclude.push("*.tmp".into());
        let second_revision = config_revision(&second).unwrap();
        let stale = save_job_in(
            &dir,
            "photos",
            &second,
            Some("photos"),
            Some("stale-revision"),
            second_revision.clone(),
        )
        .unwrap_err();
        assert_eq!(stale.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(current_revision_at(&created.path).unwrap(), first_revision);

        let updated = save_job_in(
            &dir,
            "photos",
            &second,
            Some("photos"),
            Some(&first_revision),
            second_revision.clone(),
        )
        .unwrap();
        assert_eq!(updated.effect, JobMutationEffect::Updated);
        assert_eq!(updated.job_id, created.job_id);

        let renamed = save_job_in(
            &dir,
            "archive",
            &second,
            Some("photos"),
            Some(&second_revision),
            second_revision.clone(),
        )
        .unwrap();
        assert_eq!(renamed.effect, JobMutationEffect::Renamed);
        assert_eq!(renamed.previous_name.as_deref(), Some("photos"));
        assert_eq!(renamed.job_id, created.job_id);
        assert!(!dir.join("photos.toml").exists());
        assert_eq!(current_revision_at(&renamed.path).unwrap(), second_revision);

        let stale_delete =
            delete_job_in(&dir, "archive", &renamed.job_id, &first_revision).unwrap_err();
        assert_eq!(stale_delete.kind(), std::io::ErrorKind::WouldBlock);
        let replaced_delete = delete_job_in(
            &dir,
            "archive",
            "ffffffffffffffffffffffffffffffff",
            &second_revision,
        )
        .unwrap_err();
        assert_eq!(replaced_delete.kind(), std::io::ErrorKind::WouldBlock);
        let deleted = delete_job_in(&dir, "archive", &renamed.job_id, &second_revision).unwrap();
        assert_eq!(deleted.effect, JobMutationEffect::Deleted);
        assert_eq!(deleted.job_id, renamed.job_id);
        assert!(!renamed.path.exists());

        let recreated =
            save_job_in(&dir, "archive", &first, None, None, first_revision.clone()).unwrap();
        assert_ne!(
            recreated.job_id, deleted.job_id,
            "delete and recreate is a new logical job"
        );
        assert!(std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .all(|entry| !crate::fs::staged::is_temp_name(&entry.file_name().to_string_lossy())));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn create_root_mutation_fixture(tag: &str) -> (PathBuf, SavedJob, String) {
        let dir = std::env::temp_dir().join(format!(
            "syncdash-job-root-{tag}-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let job = Job {
            source: "/data/source".into(),
            targets: vec!["/data/one".into(), "/data/two".into()],
            ..Default::default()
        };
        let revision = config_revision(&job).unwrap();
        let saved = save_job_in(&dir, "photos", &job, None, None, revision.clone()).unwrap();
        (dir, saved, revision)
    }

    #[test]
    fn root_mutations_fence_job_identity_revision_and_target_index() {
        let (dir, saved, revision) = create_root_mutation_fixture("fencing");
        let wrong_identity = update_job_root_in(
            &dir,
            "photos",
            "ffffffffffffffffffffffffffffffff",
            &revision,
            0,
            JobRootField::Source,
            "/data/revised",
        )
        .unwrap_err();
        assert_eq!(wrong_identity.kind(), std::io::ErrorKind::WouldBlock);

        let stale_revision = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            "stale-revision",
            0,
            JobRootField::Source,
            "/data/revised",
        )
        .unwrap_err();
        assert_eq!(stale_revision.kind(), std::io::ErrorKind::WouldBlock);

        let missing_target = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &revision,
            2,
            JobRootField::Target,
            "/data/revised",
        )
        .unwrap_err();
        assert_eq!(missing_target.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(current_revision_at(&saved.path).unwrap(), revision);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn root_updates_preserve_multi_target_storage_and_report_no_op() {
        let (dir, saved, revision) = create_root_mutation_fixture("update");
        let updated = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &revision,
            1,
            JobRootField::Target,
            "/data/revised-two",
        )
        .unwrap();
        assert_eq!(updated.mutation.effect, JobMutationEffect::Updated);
        assert_eq!(
            updated.targets,
            vec!["/data/one".to_string(), "/data/revised-two".to_string()]
        );
        let (_, persisted) = load_path(&updated.mutation.path).unwrap();
        assert_eq!(persisted.targets, updated.targets);

        let no_op = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &updated.mutation.config_revision,
            1,
            JobRootField::Target,
            "/data/revised-two",
        )
        .unwrap();
        assert_eq!(no_op.mutation.effect, JobMutationEffect::NoOp);
        assert_eq!(
            no_op.mutation.config_revision,
            updated.mutation.config_revision
        );

        let source_updated = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &no_op.mutation.config_revision,
            0,
            JobRootField::Source,
            "/data/revised-source",
        )
        .unwrap();
        assert_eq!(source_updated.source, "/data/revised-source");
        assert_eq!(source_updated.targets, no_op.targets);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn root_swap_is_atomic_and_root_updates_are_fully_validated() {
        let (dir, saved, revision) = create_root_mutation_fixture("swap");
        let swapped = swap_job_roots_in(&dir, "photos", &saved.job_id, &revision, 1).unwrap();
        assert_eq!(swapped.source, "/data/two");
        assert_eq!(
            swapped.targets,
            vec!["/data/one".to_string(), "/data/source".to_string()]
        );
        assert_eq!(swapped.mutation.effect, JobMutationEffect::Updated);

        let empty = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &swapped.mutation.config_revision,
            0,
            JobRootField::Source,
            "  ",
        )
        .unwrap_err();
        assert_eq!(empty.kind(), std::io::ErrorKind::InvalidInput);

        let duplicate = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &swapped.mutation.config_revision,
            0,
            JobRootField::Target,
            "/data/source",
        )
        .unwrap_err();
        assert_eq!(duplicate.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            current_revision_at(&swapped.mutation.path).unwrap(),
            swapped.mutation.config_revision
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn single_target_root_mutations_keep_canonical_list_storage() {
        let dir = std::env::temp_dir().join(format!(
            "syncdash-job-root-single-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let job = valid_job();
        let revision = config_revision(&job).unwrap();
        let saved = save_job_in(&dir, "photos", &job, None, None, revision.clone()).unwrap();

        let updated = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &revision,
            0,
            JobRootField::Target,
            "/data/revised-target",
        )
        .unwrap();
        assert_eq!(updated.targets, vec!["/data/revised-target"]);
        let (_, persisted_update) = load_path(&updated.mutation.path).unwrap();
        assert_eq!(persisted_update.targets, vec!["/data/revised-target"]);

        let swapped = swap_job_roots_in(
            &dir,
            "photos",
            &saved.job_id,
            &updated.mutation.config_revision,
            0,
        )
        .unwrap();
        assert_eq!(swapped.source, "/data/revised-target");
        assert_eq!(swapped.targets, vec!["/data/source"]);
        let (_, persisted_swap) = load_path(&swapped.mutation.path).unwrap();
        assert_eq!(persisted_swap.targets, vec!["/data/source"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn create_and_rename_never_overwrite_an_existing_job() {
        let dir = std::env::temp_dir().join(format!(
            "syncdash-job-collision-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let job = valid_job();
        let revision = config_revision(&job).unwrap();
        let one = save_job_in(&dir, "one", &job, None, None, revision.clone()).unwrap();
        save_job_in(&dir, "two", &job, None, None, revision.clone()).unwrap();

        let create = save_job_in(&dir, "one", &job, None, None, revision.clone()).unwrap_err();
        assert_eq!(create.kind(), std::io::ErrorKind::AlreadyExists);
        let identified = Job {
            job_id: one.job_id,
            ..job
        };
        let rename = save_job_in(
            &dir,
            "two",
            &identified,
            Some("one"),
            Some(&revision),
            revision.clone(),
        )
        .unwrap_err();
        assert_eq!(rename.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(dir.join("one.toml").is_file());
        assert!(dir.join("two.toml").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod rigor_tests {
    use super::*;

    fn job(rigor: &str) -> Job {
        Job {
            rigor: rigor.into(),
            ..Default::default()
        }
    }

    #[test]
    fn job_resolution_applies_detail_overrides() {
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
    }
}
