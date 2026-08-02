//! Run log (the data behind FFS's Log column / "Last sync" column).
//!
//! - Index: `<log_dir>/runs.jsonl` — one line per run (append-only, human-readable and auditable).
//! - Detail: `<log_dir>/<YYYYMMDD-HHMMSS>-<job>-<kind>/` — one directory per apply-class run:
//!   - `summary.json` run summary (= the same record as the index line, so the detail still explains itself if the index is corrupt)
//!   - `plan.jsonl`   the plan: what this run **intended** to do
//!   - `run.jsonl`    the event stream: narration, phase boundaries, errors
//!   - `errors.jsonl` the error detail: Error plus Log at warning level and above
//!   - `items.jsonl`  the execution detail: what this run **actually** did (one op per line with its outcome)
//! - compare has no side effects: one index line, no directory (a watch round every 30s = 2880 a day).
//! - Writing logs must never fail a sync: all persistence is best-effort, failures go to stderr.
//!
//! Two critical changes in v0.10 relative to v0.9:
//! 1. **Streaming**: events are persisted as they arrive (`logging::FileSink`) instead of written in
//!    one go at `finish` — v0.9 lost the entire log when the process was killed.
//! 2. **Plan and execution kept apart**: what v0.9 wrote into the detail were the **planned ops**
//!    handed to apply, with not one word on which succeeded, failed or were KEPT. Now plan and items are separate files.

use crate::model::event::{LogLevel, ProgressEvent};
use crate::model::plan::Op;
use crate::obs::logging::{self, FileSink};
use crate::obs::progress::{ApplyOutcome, ProgressSink, RunCtx};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::foundation::names::{
    RUNLOG_ERRORS_FILE, RUNLOG_INDEX_FILE as INDEX_FILE, RUNLOG_INDEX_LOCK_FILE, RUNLOG_ITEMS_FILE,
    RUNLOG_LEGACY_INDEX_FILE, RUNLOG_LEGACY_SUMMARY_FILE, RUNLOG_PLAN_FILE as PLAN_FILE,
    RUNLOG_RUN_FILE, RUNLOG_SCHEMA_FILE, RUNLOG_SUMMARY_FILE as SUMMARY_FILE,
};
use crate::foundation::path::{EntryName, RootRelativeDir, RootRelativePath};
use crate::fs::local_root::{LocalDirectory, LocalRoot};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub enum LogArtifactKind {
    Run,
    Errors,
    Items,
    Plan,
    Summary,
}

impl LogArtifactKind {
    fn file_name(self) -> &'static str {
        match self {
            Self::Run => RUNLOG_RUN_FILE,
            Self::Errors => RUNLOG_ERRORS_FILE,
            Self::Items => RUNLOG_ITEMS_FILE,
            Self::Plan => PLAN_FILE,
            Self::Summary => SUMMARY_FILE,
        }
    }
}

const RUN_RECORD_SCHEMA: u32 = 2;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub enum RunKind {
    Apply,
    PeerApply,
    Compare,
    PeerCompare,
}

impl RunKind {
    fn is_compare(self) -> bool {
        matches!(self, Self::Compare | Self::PeerCompare)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::PeerApply => "peer-apply",
            Self::Compare => "compare",
            Self::PeerCompare => "peer-compare",
        }
    }
}

impl std::fmt::Display for RunKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub enum RunJobBinding {
    Registered { job_id: String },
    AdHoc,
    LegacyUnbound,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct RunSubject {
    pub job_name: String,
    pub binding: RunJobBinding,
    #[ts(type = "number | null")]
    pub target_index: Option<usize>,
}

impl RunSubject {
    pub fn for_job(job_name: &str, job: &crate::job::SingleTargetJob) -> Self {
        let job_id = &job.configuration().job_id;
        Self {
            job_name: job_name.to_owned(),
            binding: if job_id.is_empty() {
                RunJobBinding::AdHoc
            } else {
                RunJobBinding::Registered {
                    job_id: job_id.clone(),
                }
            },
            target_index: Some(job.target_index()),
        }
    }

    pub fn registered(job_name: &str, job_id: &str, target_index: usize) -> Self {
        Self {
            job_name: job_name.to_owned(),
            binding: RunJobBinding::Registered {
                job_id: job_id.to_owned(),
            },
            target_index: Some(target_index),
        }
    }

    fn registered_job_id(&self) -> Option<&str> {
        match &self.binding {
            RunJobBinding::Registered { job_id } => Some(job_id),
            RunJobBinding::AdHoc | RunJobBinding::LegacyUnbound => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub enum RunArtifacts {
    Directory { run_id: String },
    LegacyFile { file_name: String },
    SummaryOnly,
    Unavailable,
}

impl RunArtifacts {
    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::Directory { run_id } => Some(run_id),
            Self::LegacyFile { .. } | Self::SummaryOnly | Self::Unavailable => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct RunRecord {
    #[ts(type = "number")]
    pub schema: u32,
    pub record_id: String,
    /// When the run started (unix ms)
    #[ts(type = "number")]
    pub ts_ms: i64,
    pub subject: RunSubject,
    pub kind: RunKind,
    #[ts(type = "number")]
    pub done: u64,
    #[ts(type = "number")]
    pub skipped: u64,
    #[ts(type = "number")]
    pub errors: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub elapsed_ms: u64,
    pub cancelled: bool,
    pub artifacts: RunArtifacts,
    /// How many warnings are in the error detail (the error count is in `errors`)
    #[ts(type = "number")]
    pub warnings: u64,
    /// compare-class: how many differences were found. None for apply-class
    #[ts(type = "number | null")]
    pub ops_found: Option<u64>,
    /// Whether the run went all the way through. `start` first writes a `finished:false` summary and
    /// `finish` overwrites it with true — a run killed midway has no index line (`finish` never ran),
    /// only a directory; this field is what lets that directory still say "I did not finish".
    pub finished: bool,
}

#[derive(Serialize, Clone, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct LatestRunRecord {
    pub job_id: String,
    pub record: RunRecord,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRunRecord {
    ts_ms: i64,
    job: String,
    kind: String,
    done: u64,
    skipped: u64,
    errors: u64,
    bytes: u64,
    elapsed_ms: u64,
    cancelled: bool,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    warnings: u64,
    #[serde(default)]
    ops_found: Option<u64>,
    #[serde(default = "legacy_finished")]
    finished: bool,
    #[serde(default)]
    detail: Option<String>,
}

fn legacy_finished() -> bool {
    true
}

pub fn logs_dir() -> PathBuf {
    crate::store::settings::load().resolved_log_dir()
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn run_identifier(value: &str) -> Result<EntryName, String> {
    EntryName::try_from(value).map_err(|error| format!("Invalid run identifier: {error}"))
}

fn reveal_run_identifier(value: &str) -> Result<EntryName, String> {
    let run_id = run_identifier(value)?;
    let bytes = run_id.as_str().as_bytes();
    let timestamp_shape = bytes.len() > 16
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'-'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'-';
    if timestamp_shape
        && run_id.as_str()[16..]
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
    {
        Ok(run_id)
    } else {
        Err(format!(
            "Invalid run identifier {:?}: expected a timestamped run-directory name",
            run_id.as_str()
        ))
    }
}

fn root_directory() -> RootRelativeDir {
    RootRelativeDir::try_from("").expect("the empty relative directory denotes the root")
}

fn index_relative_path() -> RootRelativePath {
    RootRelativePath::try_from(INDEX_FILE).expect("the run index name is a valid relative path")
}

fn legacy_index_relative_path() -> RootRelativePath {
    RootRelativePath::try_from(RUNLOG_LEGACY_INDEX_FILE)
        .expect("the legacy run index name is a valid relative path")
}

fn index_lock_relative_path() -> RootRelativePath {
    RootRelativePath::try_from(RUNLOG_INDEX_LOCK_FILE)
        .expect("the run-index lock name is a valid relative path")
}

fn schema_relative_path() -> RootRelativePath {
    RootRelativePath::try_from(RUNLOG_SCHEMA_FILE)
        .expect("the run schema marker name is a valid relative path")
}

fn artifact_relative_path(run_id: &EntryName, artifact: LogArtifactKind) -> RootRelativePath {
    RootRelativePath::try_from(format!("{run_id}/{}", artifact.file_name()))
        .expect("validated run and artifact names form a valid relative path")
}

fn legacy_detail_relative_path(detail: &EntryName) -> RootRelativePath {
    RootRelativePath::try_from(detail.as_str())
        .expect("a validated detail entry is a valid relative path")
}

fn legacy_summary_relative_path(run_id: &EntryName) -> RootRelativePath {
    RootRelativePath::try_from(format!("{run_id}/{RUNLOG_LEGACY_SUMMARY_FILE}"))
        .expect("validated run and summary names form a valid relative path")
}

fn random_record_id() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| std::io::Error::other(format!("cannot create a run identity: {error}")))?;
    Ok(hex_bytes(&bytes))
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}

fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn validate_hex_identity(value: &str, label: &str) -> std::io::Result<()> {
    if value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "{label} must be 32 lowercase hexadecimal characters"
        )))
    }
}

fn validate_record(record: &RunRecord) -> std::io::Result<()> {
    if record.schema != RUN_RECORD_SCHEMA {
        return Err(invalid_data(format!(
            "run record schema v{} is not supported (expected v{RUN_RECORD_SCHEMA})",
            record.schema
        )));
    }
    validate_hex_identity(&record.record_id, "record_id")?;
    if record.subject.job_name.trim().is_empty() {
        return Err(invalid_data("run record job_name cannot be empty"));
    }
    if let RunJobBinding::Registered { job_id } = &record.subject.binding {
        validate_hex_identity(job_id, "registered run job_id")?;
        if record.subject.target_index.is_none() {
            return Err(invalid_data(
                "a registered run record must identify its target index",
            ));
        }
    }
    match &record.artifacts {
        RunArtifacts::Directory { run_id } => {
            run_identifier(run_id).map_err(invalid_data)?;
        }
        RunArtifacts::LegacyFile { file_name } => {
            EntryName::try_from(file_name.as_str()).map_err(|error| {
                invalid_data(format!("invalid legacy run artifact name: {error}"))
            })?;
        }
        RunArtifacts::SummaryOnly | RunArtifacts::Unavailable => {}
    }
    if record.kind.is_compare() {
        if record.ops_found.is_none()
            || !record.finished
            || !matches!(record.artifacts, RunArtifacts::SummaryOnly)
        {
            return Err(invalid_data(
                "compare records must be finished summary-only records with ops_found",
            ));
        }
    } else {
        if record.ops_found.is_some()
            || matches!(record.artifacts, RunArtifacts::SummaryOnly)
            || (!record.finished && !matches!(record.artifacts, RunArtifacts::Directory { .. }))
        {
            return Err(invalid_data(
                "apply records must not carry compare counts, and interrupted applies require a run directory",
            ));
        }
    }
    Ok(())
}

fn parse_current_record(raw: &str, context: &str) -> std::io::Result<RunRecord> {
    let record = serde_json::from_str::<RunRecord>(raw)
        .map_err(|error| invalid_data(format!("invalid {context}: {error}")))?;
    validate_record(&record)
        .map_err(|error| invalid_data(format!("invalid {context}: {error}")))?;
    Ok(record)
}

fn legacy_record_id(raw: &str, discriminator: &str) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(b"syncdash-run-record-v2-legacy\0");
    digest.update(discriminator.as_bytes());
    digest.update(b"\0");
    digest.update(raw.as_bytes());
    hex_bytes(&digest.finalize().as_bytes()[..16])
}

fn legacy_kind(kind: &str) -> std::io::Result<RunKind> {
    match kind {
        "apply" => Ok(RunKind::Apply),
        "remote-apply" => Ok(RunKind::PeerApply),
        "compare" => Ok(RunKind::Compare),
        "remote-compare" => Ok(RunKind::PeerCompare),
        _ => Err(invalid_data(format!(
            "legacy run kind '{kind}' is not recognized"
        ))),
    }
}

fn registered_job_bindings() -> std::io::Result<HashMap<String, String>> {
    crate::job::load_all().map(|jobs| {
        jobs.into_iter()
            .filter(|(_, job)| job.targets.len() == 1)
            .map(|(name, job)| (name, job.job_id))
            .collect()
    })
}

fn migrate_legacy_record(
    legacy: LegacyRunRecord,
    record_id: String,
    bindings: &HashMap<String, String>,
) -> std::io::Result<RunRecord> {
    let kind = legacy_kind(&legacy.kind)?;
    let binding = bindings
        .get(&legacy.job)
        .map_or(RunJobBinding::LegacyUnbound, |job_id| {
            RunJobBinding::Registered {
                job_id: job_id.clone(),
            }
        });
    let target_index = matches!(binding, RunJobBinding::Registered { .. }).then_some(0);
    let artifacts = match (legacy.run_id, legacy.detail, kind.is_compare()) {
        (Some(_), Some(_), _) => {
            return Err(invalid_data(
                "legacy run record names both a run directory and a flat detail file",
            ))
        }
        (Some(run_id), None, false) => RunArtifacts::Directory { run_id },
        (None, Some(file_name), false) => RunArtifacts::LegacyFile { file_name },
        (None, None, true) => RunArtifacts::SummaryOnly,
        (None, None, false) => RunArtifacts::Unavailable,
        (Some(_), None, true) | (None, Some(_), true) => {
            return Err(invalid_data(
                "legacy compare record unexpectedly names detail artifacts",
            ))
        }
    };
    let record = RunRecord {
        schema: RUN_RECORD_SCHEMA,
        record_id,
        ts_ms: legacy.ts_ms,
        subject: RunSubject {
            job_name: legacy.job,
            binding,
            target_index,
        },
        kind,
        done: legacy.done,
        skipped: legacy.skipped,
        errors: legacy.errors,
        bytes: legacy.bytes,
        elapsed_ms: legacy.elapsed_ms,
        cancelled: legacy.cancelled,
        artifacts,
        warnings: legacy.warnings,
        ops_found: legacy.ops_found,
        finished: legacy.finished,
    };
    validate_record(&record)?;
    Ok(record)
}

fn write_staged_text(
    root: &LocalRoot,
    relative: &RootRelativePath,
    text: &str,
    no_replace: bool,
) -> std::io::Result<()> {
    let mut staged = root.create_staged(relative)?;
    staged.write_all(text.as_bytes())?;
    staged.seal(true)?;
    if no_replace {
        staged.commit_noreplace()
    } else {
        staged.commit()
    }
}

fn ensure_immutable_backup(
    root: &LocalRoot,
    relative: &RootRelativePath,
    source: &str,
) -> std::io::Result<()> {
    match root.read_to_string(relative) {
        Ok(existing) if existing == source => Ok(()),
        Ok(_) => Err(invalid_data(format!(
            "migration backup '{relative}' does not match its source"
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match write_staged_text(root, relative, source, true) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = root.read_to_string(relative)?;
                    if existing == source {
                        Ok(())
                    } else {
                        Err(invalid_data(format!(
                            "migration backup '{relative}' was concurrently published with different content"
                        )))
                    }
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn migrate_summaries_locked(
    root: &LocalRoot,
    indexed_records: &[RunRecord],
    bindings: &HashMap<String, String>,
) -> std::io::Result<()> {
    let indexed_by_run_id: HashMap<&str, &RunRecord> = indexed_records
        .iter()
        .filter_map(|record| record.artifacts.run_id().map(|run_id| (run_id, record)))
        .collect();
    for entry in root.read_directory(&root_directory())? {
        if !entry.metadata.is_dir() || reveal_run_identifier(entry.name.as_str()).is_err() {
            continue;
        }
        let summary_path = artifact_relative_path(&entry.name, LogArtifactKind::Summary);
        let Some(raw) = read_optional_text(root, &summary_path)? else {
            continue;
        };
        if let Ok(current) = serde_json::from_str::<RunRecord>(&raw) {
            validate_record(&current)?;
            if current.artifacts.run_id() != Some(entry.name.as_str()) {
                return Err(invalid_data(format!(
                    "run summary in '{}' names a different artifact directory",
                    entry.name
                )));
            }
            continue;
        }
        let legacy = serde_json::from_str::<LegacyRunRecord>(&raw).map_err(|error| {
            invalid_data(format!(
                "invalid legacy summary in '{}': {error}",
                entry.name
            ))
        })?;
        let record_id = indexed_by_run_id.get(entry.name.as_str()).map_or_else(
            || legacy_record_id(&raw, entry.name.as_str()),
            |record| record.record_id.clone(),
        );
        let mut migrated = migrate_legacy_record(legacy, record_id, bindings)?;
        migrated.artifacts = RunArtifacts::Directory {
            run_id: entry.name.as_str().to_owned(),
        };
        validate_record(&migrated)?;
        ensure_immutable_backup(root, &legacy_summary_relative_path(&entry.name), &raw)?;
        let current = serde_json::to_string_pretty(&migrated)?;
        write_staged_text(root, &summary_path, &current, false)?;
    }
    Ok(())
}

fn ensure_current_schema_locked(root: &LocalRoot) -> std::io::Result<()> {
    match root.read_to_string(&schema_relative_path()) {
        Ok(schema) if schema == format!("{RUN_RECORD_SCHEMA}\n") => return Ok(()),
        Ok(schema) => {
            return Err(invalid_data(format!(
                "run schema marker contains unsupported value {:?}",
                schema.trim_end()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let bindings = registered_job_bindings()?;
    migrate_current_schema_locked(root, &bindings)
}

fn migrate_current_schema_locked(
    root: &LocalRoot,
    bindings: &HashMap<String, String>,
) -> std::io::Result<()> {
    let Some(index_text) = read_optional_text(root, &index_relative_path())? else {
        migrate_summaries_locked(root, &[], bindings)?;
        return write_staged_text(
            root,
            &schema_relative_path(),
            &format!("{RUN_RECORD_SCHEMA}\n"),
            true,
        );
    };
    let lines: Vec<_> = index_text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .collect();
    let current_count = lines
        .iter()
        .filter(|(_, line)| serde_json::from_str::<RunRecord>(line.trim()).is_ok())
        .count();
    if current_count > 0 && current_count != lines.len() {
        return Err(invalid_data(
            "run index mixes current and legacy records; refusing an ambiguous migration",
        ));
    }
    if current_count == lines.len() {
        let records = lines
            .iter()
            .map(|(line_index, line)| {
                parse_current_record(line.trim(), &format!("run index line {}", line_index + 1))
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        migrate_summaries_locked(root, &records, bindings)?;
        return write_staged_text(
            root,
            &schema_relative_path(),
            &format!("{RUN_RECORD_SCHEMA}\n"),
            true,
        );
    }

    let records = lines
        .iter()
        .map(|(line_index, line)| {
            let raw = line.trim();
            let legacy = serde_json::from_str::<LegacyRunRecord>(raw).map_err(|error| {
                invalid_data(format!(
                    "invalid legacy run index line {}: {error}",
                    line_index + 1
                ))
            })?;
            migrate_legacy_record(
                legacy,
                legacy_record_id(raw, &line_index.to_string()),
                bindings,
            )
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure_immutable_backup(root, &legacy_index_relative_path(), &index_text)?;
    migrate_summaries_locked(root, &records, bindings)?;
    let mut migrated_index = String::new();
    for record in &records {
        migrated_index.push_str(&serde_json::to_string(record)?);
        migrated_index.push('\n');
    }
    write_staged_text(root, &index_relative_path(), &migrated_index, false)?;
    write_staged_text(
        root,
        &schema_relative_path(),
        &format!("{RUN_RECORD_SCHEMA}\n"),
        true,
    )
}

fn with_index_lock<T>(
    root: &LocalRoot,
    operation: impl FnOnce(&LocalRoot) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let lock = root.open_lock_file(&index_lock_relative_path())?;
    lock.lock()?;
    ensure_current_schema_locked(root)?;
    operation(root)
}

static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);

fn create_run_dir(
    root: &LocalRoot,
    ts_ms: i64,
    name: &str,
    kind: RunKind,
) -> std::io::Result<(String, LocalDirectory)> {
    let root_directory = root.open_directory(&root_directory())?;
    loop {
        let sequence = NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed);
        let run_id = format!(
            "{}-{:03}-{}-{}-{}-{sequence}",
            crate::foundation::time::stamp_compact(ts_ms),
            ts_ms.rem_euclid(1_000),
            sanitize(name),
            kind.as_str(),
            std::process::id(),
        );
        let entry_name = EntryName::try_from(run_id.as_str())
            .expect("generated run identifiers satisfy the entry-name contract");
        match root_directory.create_child_directory(&entry_name) {
            Ok(directory) => return Ok((run_id, directory)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Tallies the makeup of the error detail in passing — the source of summary's "3 warnings, 2 errors".
#[derive(Default)]
struct Tally {
    warnings: AtomicU64,
    errors: AtomicU64,
}

struct TallySink(Arc<Tally>);

impl ProgressSink for TallySink {
    fn emit(&self, ev: ProgressEvent) {
        match &ev {
            ProgressEvent::Log {
                level: LogLevel::Warn,
                ..
            } => {
                self.0.warnings.fetch_add(1, Ordering::Relaxed);
            }
            ProgressEvent::Log {
                level: LogLevel::Error,
                ..
            }
            | ProgressEvent::Error { .. } => {
                self.0.errors.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// Recorder for one apply-class run.
///
/// `start` threads the file sink into the event stream **and installs it in the process registry** —
/// the latter is what lets the diagnostics in `trash` / `version` / `lock`, which have no `RunCtx`, land in the same directory.
/// `finish` only writes the summary and the index: the detail was already persisted as the run went.
pub struct Recorder {
    pub ctx: RunCtx,
    subject: RunSubject,
    kind: RunKind,
    record_id: Option<String>,
    ts_ms: i64,
    root: Option<LocalRoot>,
    dir: Option<LocalDirectory>,
    run_id: Option<String>,
    file: Option<Arc<FileSink>>,
    tally: Arc<Tally>,
    /// Restores the registry automatically on drop — leaking the guard cross-contaminates this directory with the next run's log
    _guard: Option<crate::obs::progress::SinkGuard>,
}

impl Recorder {
    pub fn start(subject: RunSubject, kind: RunKind, base: &RunCtx, ops: &[Op]) -> Recorder {
        let cfg = crate::store::settings::load();
        let ts_ms = crate::foundation::time::now_ms() as i64;
        let record_id = match random_record_id() {
            Ok(record_id) => Some(record_id),
            Err(error) => {
                eprintln!("runlog: {error}");
                None
            }
        };
        let root_path = cfg.resolved_log_dir();
        let root = match LocalRoot::create(root_path.clone())
            .and_then(|root| with_index_lock(&root, |_| Ok(())).map(|()| root))
        {
            Ok(root) if record_id.is_some() => Some(root),
            Ok(_) => None,
            Err(error) => {
                eprintln!(
                    "runlog: cannot prepare the configured log root {}: {error}",
                    root_path.display()
                );
                None
            }
        };
        let tally = Arc::new(Tally::default());

        // Do not thread `progress::current()` in here: `RunCtx::null()` already brought the process's
        // ambient sink (the StderrSink the CLI installs at startup) into `base.sink`; threading it again duplicates output.
        let mut sinks: Vec<Arc<dyn ProgressSink>> =
            vec![base.sink.clone(), Arc::new(TallySink(tally.clone()))];
        let run_directory = root
            .as_ref()
            .map(|root| create_run_dir(root, ts_ms, &subject.job_name, kind));
        let (file, run_id, dir) = match run_directory {
            Some(Ok((run_id, dir))) => {
                write_plan(&dir, ops);
                // Write a finished:false summary up front — so even if the process is killed, this
                // directory can still say "who I am and that I did not finish" instead of becoming anonymous debris
                write_summary(
                    &dir,
                    &pending_record(
                        record_id
                            .as_deref()
                            .expect("a prepared run-log root requires a record identity"),
                        &subject,
                        kind,
                        ts_ms,
                        &run_id,
                        ops.len() as u64,
                    ),
                );
                let f = Arc::new(FileSink::open_in(&dir, cfg.level));
                sinks.push(f.clone() as Arc<dyn ProgressSink>);
                (Some(f), Some(run_id), Some(dir))
            }
            Some(Err(e)) => {
                // A directory we cannot create costs us the detail, not the sync — the index line is still written
                eprintln!(
                    "runlog: cannot create a run directory under {}: {e}",
                    root_path.display()
                );
                (None, None, None)
            }
            None => (None, None, None),
        };

        let sink: Arc<dyn ProgressSink> = Arc::new(logging::MultiSink::new(sinks));
        let guard = crate::obs::progress::install(sink.clone());
        Recorder {
            ctx: RunCtx::new(base.ctl.clone(), sink),
            subject,
            kind,
            record_id,
            ts_ms,
            root,
            dir,
            run_id,
            file,
            tally,
            _guard: Some(guard),
        }
    }

    /// Returns the record when a cryptographic record identity was available; persistence remains best-effort.
    pub fn finish(self, out: &ApplyOutcome, elapsed_ms: u64) -> Option<RunRecord> {
        // Flush every buffer first, then write the summary — the summary existing means the detail is complete
        if let Some(f) = &self.file {
            f.flush_all();
        }
        let record_id = self.record_id?;
        let rec = RunRecord {
            schema: RUN_RECORD_SCHEMA,
            record_id,
            ts_ms: self.ts_ms,
            subject: self.subject,
            kind: self.kind,
            done: out.done,
            skipped: out.skipped,
            errors: out.errors,
            bytes: out.bytes_copied,
            elapsed_ms,
            cancelled: out.cancelled,
            artifacts: self
                .run_id
                .clone()
                .map_or(RunArtifacts::Unavailable, |run_id| {
                    RunArtifacts::Directory { run_id }
                }),
            warnings: self.tally.warnings.load(Ordering::Relaxed),
            ops_found: None,
            finished: true,
        };
        if let Some(dir) = &self.dir {
            // Overwrite the finished:false placeholder written by start
            write_summary(dir, &rec);
        }
        if let Some(root) = &self.root {
            append_index(root, &rec);
        }
        Some(rec)
    }
}

/// Placeholder summary written at the start of a run: the plan size is known, the results are empty, `finished:false`.
fn pending_record(
    record_id: &str,
    subject: &RunSubject,
    kind: RunKind,
    ts_ms: i64,
    run_id: &str,
    planned: u64,
) -> RunRecord {
    RunRecord {
        schema: RUN_RECORD_SCHEMA,
        record_id: record_id.to_owned(),
        ts_ms,
        subject: subject.clone(),
        kind,
        done: 0,
        skipped: planned,
        errors: 0,
        bytes: 0,
        elapsed_ms: 0,
        cancelled: false,
        artifacts: RunArtifacts::Directory {
            run_id: run_id.to_owned(),
        },
        warnings: 0,
        ops_found: None,
        finished: false,
    }
}

fn write_summary(dir: &LocalDirectory, rec: &RunRecord) {
    match serde_json::to_string_pretty(rec) {
        Ok(t) => {
            let result = (|| -> std::io::Result<()> {
                let name = EntryName::try_from(SUMMARY_FILE)
                    .expect("the summary artifact name is a valid entry");
                let mut staged = dir.create_staged(name)?;
                staged.write_all(t.as_bytes())?;
                staged.seal(true)?;
                staged.commit()
            })();
            if let Err(e) = result {
                eprintln!("runlog: cannot write the run summary: {e}");
            }
        }
        Err(e) => eprintln!("runlog: summary serialization failed: {e}"),
    }
}

fn write_plan(dir: &LocalDirectory, ops: &[Op]) {
    let write = || -> std::io::Result<()> {
        let name = EntryName::try_from(PLAN_FILE).expect("the plan artifact name is a valid entry");
        let f = dir.create_new_file(&name)?;
        let mut w = std::io::BufWriter::new(f);
        for op in ops {
            writeln!(w, "{}", serde_json::json!({ "op": op }))?;
        }
        w.flush()
    };
    if let Err(e) = write() {
        eprintln!("runlog: writing the plan failed: {e}");
    }
}

fn append_index(root: &LocalRoot, rec: &RunRecord) {
    let append = || -> std::io::Result<()> {
        validate_record(rec)?;
        with_index_lock(root, |root| {
            let relative = index_relative_path();
            let mut file = root.open_append(&relative)?;
            writeln!(file, "{}", serde_json::to_string(rec)?)?;
            file.sync_all()?;
            root.sync_parent(&relative)
        })
    };
    if let Err(e) = append() {
        eprintln!("runlog: appending to the index failed: {e}");
    }
}

/// The trace a compare-class run leaves: append one index line, **no directory**.
///
/// A watch round every 30s = 2880 a day, and creating a directory each time would flood the log disk;
/// the single line "when we compared and how many differences we found" is worth keeping on its own.
pub fn compare_summary(
    subject: RunSubject,
    kind: RunKind,
    ts_ms: i64,
    ops_found: u64,
    elapsed_ms: u64,
    cancelled: bool,
) {
    let settings = crate::store::settings::load();
    if !settings.logs_compare() {
        return;
    }
    let root_path = settings.resolved_log_dir();
    let record = RunRecord {
        schema: RUN_RECORD_SCHEMA,
        record_id: match random_record_id() {
            Ok(record_id) => record_id,
            Err(error) => {
                eprintln!("runlog: {error}");
                return;
            }
        },
        ts_ms,
        subject,
        kind,
        done: 0,
        skipped: 0,
        errors: 0,
        bytes: 0,
        elapsed_ms,
        cancelled,
        artifacts: RunArtifacts::SummaryOnly,
        warnings: 0,
        ops_found: Some(ops_found),
        finished: true,
    };
    match LocalRoot::create(root_path) {
        Ok(root) => append_index(&root, &record),
        Err(error) => eprintln!("runlog: opening the log root failed: {error}"),
    }
}

fn open_existing_log_root() -> std::io::Result<Option<LocalRoot>> {
    match LocalRoot::open(logs_dir()) {
        Ok(root) => Ok(Some(root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Lend an ambient spelling to one presentation callback after descriptor-relative validation.
/// The callback must not reuse the spelling as read/write authority; a concurrent namespace rename
/// can still make the operating system's later file-manager lookup fail or resolve differently.
pub fn with_validated_reveal_target<T>(
    record_id: Option<&str>,
    reveal: impl FnOnce(&std::path::Path) -> Result<T, String>,
) -> Result<T, String> {
    with_validated_reveal_target_at(logs_dir(), record_id, reveal)
}

fn with_validated_reveal_target_at<T>(
    root_path: PathBuf,
    record_id: Option<&str>,
    reveal: impl FnOnce(&std::path::Path) -> Result<T, String>,
) -> Result<T, String> {
    let root = LocalRoot::open(root_path)
        .map_err(|error| format!("Cannot open the log directory: {error}"))?;
    root.metadata_directory(&root_directory())
        .map_err(|error| format!("Cannot validate the log directory: {error}"))?;

    let target = match record_id {
        Some(record_id) => {
            validate_hex_identity(record_id, "record_id").map_err(|error| error.to_string())?;
            let record = with_index_lock(&root, |root| record_by_id_at(root, record_id))
                .map_err(|error| format!("Cannot resolve run record {record_id}: {error}"))?;
            match record.artifacts {
                RunArtifacts::Directory { run_id } => {
                    let run_id = reveal_run_identifier(&run_id)?;
                    let relative = RootRelativeDir::try_from(run_id.as_str())
                        .expect("a validated run identifier is a valid relative directory");
                    root.metadata_directory(&relative).map_err(|error| {
                        format!("Cannot validate run directory {run_id}: {error}")
                    })?;
                    root.display_path().join(run_id.as_str())
                }
                RunArtifacts::LegacyFile { .. }
                | RunArtifacts::SummaryOnly
                | RunArtifacts::Unavailable => root.display_path().to_path_buf(),
            }
        }
        None => root.display_path().to_path_buf(),
    };

    reveal(&target)
}

fn read_optional_text(
    root: &LocalRoot,
    relative: &RootRelativePath,
) -> std::io::Result<Option<String>> {
    match root.read_to_string(relative) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// History (newest → oldest). A corrupt current record is reported instead of being hidden.
pub fn history(job: Option<&str>, limit: usize) -> std::io::Result<Vec<RunRecord>> {
    let Some(root) = open_existing_log_root()? else {
        return Ok(Vec::new());
    };
    with_index_lock(&root, |root| history_at(root, job, limit))
}

fn history_at(
    root: &LocalRoot,
    job: Option<&str>,
    limit: usize,
) -> std::io::Result<Vec<RunRecord>> {
    let Some(text) = read_optional_text(root, &index_relative_path())? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record =
            parse_current_record(line.trim(), &format!("run index line {}", line_index + 1))?;
        if job
            .map(|job_name| record.subject.job_name == job_name)
            .unwrap_or(true)
        {
            out.push(record);
        }
    }
    out.reverse();
    out.truncate(limit);
    Ok(out)
}

/// History plus **interrupted runs** (newest → oldest).
///
/// The index line is only appended inside `finish`, so a run whose process was killed does not exist
/// in the index at all — only a directory is left. A UI that reads only the index makes crashed runs
/// completely invisible, and those are exactly the ones that most need to be seen. This merges in the
/// directory's `summary.json` (the `finished:false` placeholder written by `start`).
pub fn history_merged(job: Option<&str>, limit: usize) -> std::io::Result<Vec<RunRecord>> {
    let Some(root) = open_existing_log_root()? else {
        return Ok(Vec::new());
    };
    with_index_lock(&root, |root| history_merged_at(root, job, limit))
}

pub fn history_merged_for_registered_job(
    job_id: Option<&str>,
    limit: usize,
) -> std::io::Result<Vec<RunRecord>> {
    if let Some(job_id) = job_id {
        validate_hex_identity(job_id, "job_id")?;
    }
    let Some(root) = open_existing_log_root()? else {
        return Ok(Vec::new());
    };
    with_index_lock(&root, |root| {
        let mut records = history_merged_at(root, None, usize::MAX)?;
        if let Some(job_id) = job_id {
            records.retain(|record| record.subject.registered_job_id() == Some(job_id));
        }
        records.truncate(limit);
        Ok(records)
    })
}

fn history_merged_at(
    root: &LocalRoot,
    job: Option<&str>,
    limit: usize,
) -> std::io::Result<Vec<RunRecord>> {
    let mut out = history_at(root, job, usize::MAX)?;
    let known: std::collections::HashSet<String> = out
        .iter()
        .filter_map(|record| record.artifacts.run_id().map(str::to_owned))
        .collect();
    match root.read_directory(&root_directory()) {
        Ok(entries) => {
            for entry in entries {
                if !entry.metadata.is_dir() || reveal_run_identifier(entry.name.as_str()).is_err() {
                    continue;
                }
                if known.contains(entry.name.as_str()) {
                    continue;
                }
                let summary_path = artifact_relative_path(&entry.name, LogArtifactKind::Summary);
                let t = match root.read_to_string(&summary_path) {
                    Ok(text) => text,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                let r = parse_current_record(
                    &t,
                    &format!("run summary in directory '{}'", entry.name),
                )?;
                if r.artifacts.run_id() != Some(entry.name.as_str()) {
                    return Err(invalid_data(format!(
                        "run summary in '{}' names a different artifact directory",
                        entry.name
                    )));
                }
                if job
                    .map(|job_name| r.subject.job_name == job_name)
                    .unwrap_or(true)
                {
                    out.push(r);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    out.sort_by_key(|record| std::cmp::Reverse(record.ts_ms));
    out.truncate(limit);
    Ok(out)
}

/// The most recent run per job that **actually executed** (the sidebar's "last sync" dot).
/// compare lines do not count — they moved no data, and calling one "last sync" would be a lie.
pub fn latest_by_job() -> std::io::Result<Vec<LatestRunRecord>> {
    let mut m = HashMap::new();
    let Some(root) = open_existing_log_root()? else {
        return Ok(Vec::new());
    };
    with_index_lock(&root, |root| {
        let Some(text) = read_optional_text(root, &index_relative_path())? else {
            return Ok(Vec::new());
        };
        for (line_index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record =
                parse_current_record(line.trim(), &format!("run index line {}", line_index + 1))?;
            if record.kind.is_compare() {
                continue;
            }
            if let Some(job_id) = record.subject.registered_job_id() {
                m.insert(job_id.to_owned(), record);
            }
        }
        let mut latest: Vec<_> = m
            .into_iter()
            .map(|(job_id, record)| LatestRunRecord { job_id, record })
            .collect();
        latest.sort_by(|left, right| left.job_id.cmp(&right.job_id));
        Ok(latest)
    })
}

fn record_by_id_at(root: &LocalRoot, record_id: &str) -> std::io::Result<RunRecord> {
    let mut matches = history_merged_at(root, None, usize::MAX)?
        .into_iter()
        .filter(|record| record.record_id == record_id);
    let record = matches.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("run record '{record_id}' does not exist"),
        )
    })?;
    if matches.next().is_some() {
        return Err(invalid_data(format!(
            "run record identity '{record_id}' is duplicated"
        )));
    }
    Ok(record)
}

/// One artifact of a run (raw JSONL lines; the caller owns the line-count memory bound).
pub fn artifact_lines(
    record_id: &str,
    artifact: LogArtifactKind,
    max_lines: usize,
) -> Result<Vec<String>, String> {
    validate_hex_identity(record_id, "record_id").map_err(|error| error.to_string())?;
    let root = LocalRoot::open(logs_dir())
        .map_err(|error| format!("Cannot open the log root: {error}"))?;
    with_index_lock(&root, |root| {
        let record = record_by_id_at(root, record_id)?;
        match record.artifacts {
            RunArtifacts::Directory { run_id } => {
                let run_id = run_identifier(&run_id).map_err(invalid_data)?;
                artifact_lines_at(root, &run_id, artifact, max_lines).map_err(invalid_data)
            }
            RunArtifacts::LegacyFile { file_name } if artifact == LogArtifactKind::Run => {
                let detail = EntryName::try_from(file_name)
                    .map_err(|error| invalid_data(error.to_string()))?;
                let text = read_optional_text(root, &legacy_detail_relative_path(&detail))?
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("legacy run artifact '{detail}' does not exist"),
                        )
                    })?;
                Ok(text.lines().take(max_lines).map(str::to_owned).collect())
            }
            RunArtifacts::LegacyFile { .. } => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "this migrated legacy run has only an event-stream artifact",
            )),
            RunArtifacts::SummaryOnly => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "compare records retain summary evidence only",
            )),
            RunArtifacts::Unavailable => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "detail persistence was unavailable for this run",
            )),
        }
    })
    .map_err(|error| error.to_string())
}

fn artifact_lines_at(
    root: &LocalRoot,
    run_id: &EntryName,
    artifact: LogArtifactKind,
    max_lines: usize,
) -> Result<Vec<String>, String> {
    let relative = artifact_relative_path(run_id, artifact);
    let text = root.read_to_string(&relative).map_err(|error| {
        format!(
            "Cannot read log artifact {}/{}: {error}",
            run_id,
            artifact.file_name()
        )
    })?;
    Ok(text.lines().take(max_lines).map(str::to_string).collect())
}

fn child_directory(parent: &RootRelativeDir, name: &EntryName) -> RootRelativeDir {
    let relative = if parent.as_str().is_empty() {
        name.as_str().to_owned()
    } else {
        format!("{parent}/{name}")
    };
    RootRelativeDir::try_from(relative)
        .expect("validated directory and entry names form a valid relative directory")
}

fn directory_size(root: &LocalRoot, directory: &RootRelativeDir) -> std::io::Result<u64> {
    let mut bytes = 0;
    for entry in root.read_directory(directory)? {
        bytes += if entry.metadata.is_dir() {
            directory_size(root, &child_directory(directory, &entry.name))?
        } else {
            entry.metadata.len()
        };
    }
    Ok(bytes)
}

/// Delete one run record's detail (the old format's flat file / the new format's directory).
fn drop_detail(r: &RunRecord, root: &LocalRoot) -> std::io::Result<()> {
    if let RunArtifacts::Directory { run_id } = &r.artifacts {
        let id = run_identifier(run_id)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let directory = RootRelativeDir::try_from(id.as_str())
            .expect("a run identifier is a valid relative directory");
        match root.remove_directory_all(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if let RunArtifacts::LegacyFile { file_name } = &r.artifacts {
        let detail = EntryName::try_from(file_name.as_str()).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        match root.remove_file(&legacy_detail_relative_path(&detail)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Retention on two conditions: age in days plus total size. Returns how many runs were deleted.
///
/// `keep_days == 0` turns off the age rule; `max_total_mb == 0` turns off the size rule.
/// The execution detail records everything (tens of thousands of lines for one big sync) — the size gate is its seatbelt.
pub fn prune(keep_days: u64, max_total_mb: u64) -> std::io::Result<u64> {
    let Some(root) = open_existing_log_root()? else {
        return Ok(0);
    };
    with_index_lock(&root, |root| prune_at(root, keep_days, max_total_mb))
}

fn prune_at(root: &LocalRoot, keep_days: u64, max_total_mb: u64) -> std::io::Result<u64> {
    let text = read_optional_text(root, &index_relative_path())?.unwrap_or_default();
    let retention_ms = keep_days.saturating_mul(24 * 60 * 60 * 1_000);
    let cutoff = (crate::foundation::time::now_ms() as i64)
        .saturating_sub(retention_ms.min(i64::MAX as u64) as i64);

    struct RetentionEntry {
        record: RunRecord,
        raw: String,
        bytes: u64,
        drop: bool,
    }

    let mut entries = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        let record = serde_json::from_str::<RunRecord>(raw).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid run index line {}: {error}", line_index + 1),
            )
        })?;
        let bytes = record_detail_size(root, &record)?;
        let drop = keep_days > 0 && record.ts_ms < cutoff;
        entries.push(RetentionEntry {
            record,
            raw: raw.to_owned(),
            bytes,
            drop,
        });
    }

    if max_total_mb > 0 {
        let cap = max_total_mb.checked_mul(1024 * 1024).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "log size cap is too large",
            )
        })?;
        let mut total = entries
            .iter()
            .filter(|entry| !entry.drop)
            .try_fold(0u64, |total, entry| total.checked_add(entry.bytes))
            .ok_or_else(|| std::io::Error::other("run-log size total overflowed"))?;
        for entry in &mut entries {
            if total <= cap {
                break;
            }
            if !entry.drop
                && matches!(
                    entry.record.artifacts,
                    RunArtifacts::Directory { .. } | RunArtifacts::LegacyFile { .. }
                )
            {
                entry.drop = true;
                total = total.saturating_sub(entry.bytes);
            }
        }
    }

    let mut dropped = 0;
    for entry in &mut entries {
        if entry.drop {
            match drop_detail(&entry.record, root) {
                Ok(()) => dropped += 1,
                Err(error) => {
                    entry.drop = false;
                    eprintln!(
                        "runlog: retaining a record whose detail could not be removed: {error}"
                    );
                }
            }
        }
    }

    if dropped > 0 {
        let body: String = entries
            .iter()
            .filter(|entry| !entry.drop)
            .map(|entry| format!("{}\n", entry.raw))
            .collect();
        let mut staged = root.create_staged(&index_relative_path())?;
        staged.write_all(body.as_bytes())?;
        staged.seal(true)?;
        staged.commit()?;
    }

    if keep_days > 0 {
        let live: std::collections::HashSet<&str> = entries
            .iter()
            .filter(|entry| !entry.drop)
            .filter_map(|entry| entry.record.artifacts.run_id())
            .collect();
        sweep_orphans(root, &live, cutoff)?;
    }
    Ok(dropped)
}

fn record_detail_size(root: &LocalRoot, record: &RunRecord) -> std::io::Result<u64> {
    let mut bytes = 0u64;
    if let RunArtifacts::Directory { run_id } = &record.artifacts {
        let run_id = run_identifier(run_id)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let directory = RootRelativeDir::try_from(run_id.as_str())
            .expect("a run identifier is a valid relative directory");
        match directory_size(root, &directory) {
            Ok(size) => {
                bytes = bytes
                    .checked_add(size)
                    .ok_or_else(|| std::io::Error::other("run-log detail size overflowed"))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if let RunArtifacts::LegacyFile { file_name } = &record.artifacts {
        let detail = EntryName::try_from(file_name.as_str()).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        match root.metadata_path(&legacy_detail_relative_path(&detail)) {
            Ok(metadata) if metadata.is_file() => {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| std::io::Error::other("run-log detail size overflowed"))?;
            }
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("legacy log detail {detail} is not a regular file"),
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(bytes)
}

fn generated_run_identifier(run_id: &EntryName) -> bool {
    let value = run_id.as_str();
    let bytes = value.as_bytes();
    if bytes.len() < 24
        || !bytes[..8].iter().all(u8::is_ascii_digit)
        || bytes[8] != b'-'
        || !bytes[9..15].iter().all(u8::is_ascii_digit)
        || bytes[15] != b'-'
        || !bytes[16..19].iter().all(u8::is_ascii_digit)
        || bytes[19] != b'-'
    {
        return false;
    }
    let mut suffixes = value.rsplitn(3, '-');
    let Some(sequence) = suffixes.next() else {
        return false;
    };
    let Some(process_id) = suffixes.next() else {
        return false;
    };
    let Some(stem) = suffixes.next() else {
        return false;
    };
    !stem.is_empty()
        && !sequence.is_empty()
        && sequence.bytes().all(|byte| byte.is_ascii_digit())
        && !process_id.is_empty()
        && process_id.bytes().all(|byte| byte.is_ascii_digit())
}

fn sweep_orphans(
    root: &LocalRoot,
    live: &std::collections::HashSet<&str>,
    cutoff: i64,
) -> std::io::Result<()> {
    for entry in root.read_directory(&root_directory())? {
        if !entry.metadata.is_dir() {
            continue;
        }
        if live.contains(entry.name.as_str()) || !generated_run_identifier(&entry.name) {
            continue;
        }
        let summary_path = artifact_relative_path(&entry.name, LogArtifactKind::Summary);
        let Some(summary) = read_optional_text(root, &summary_path)? else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<RunRecord>(&summary) else {
            continue;
        };
        if record.artifacts.run_id() != Some(entry.name.as_str()) {
            continue;
        }
        let modified = entry.metadata.modified()?.into_std();
        let old_enough = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| (duration.as_millis() as i64) < cutoff)
            .unwrap_or(false);
        if !old_enough {
            continue;
        }
        let directory = RootRelativeDir::try_from(entry.name.as_str())
            .expect("a directory entry name is a valid relative directory");
        root.remove_directory_all(&directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD_A: &str = "0123456789abcdef0123456789abcdef";
    const RECORD_B: &str = "fedcba9876543210fedcba9876543210";

    fn test_subject(job_name: &str) -> RunSubject {
        RunSubject {
            job_name: job_name.to_owned(),
            binding: RunJobBinding::LegacyUnbound,
            target_index: None,
        }
    }

    fn test_apply_record(record_id: &str, run_id: &str, finished: bool) -> RunRecord {
        RunRecord {
            schema: RUN_RECORD_SCHEMA,
            record_id: record_id.to_owned(),
            ts_ms: 1,
            subject: test_subject("job"),
            kind: RunKind::Apply,
            done: 0,
            skipped: 0,
            errors: 0,
            bytes: 0,
            elapsed_ms: 0,
            cancelled: false,
            artifacts: RunArtifacts::Directory {
                run_id: run_id.to_owned(),
            },
            warnings: 0,
            ops_found: None,
            finished,
        }
    }

    fn write_current_index(root_path: &std::path::Path, records: &[RunRecord]) {
        let index = records
            .iter()
            .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
            .collect::<String>();
        std::fs::write(root_path.join(INDEX_FILE), index).unwrap();
        std::fs::write(
            root_path.join(RUNLOG_SCHEMA_FILE),
            format!("{RUN_RECORD_SCHEMA}\n"),
        )
        .unwrap();
    }

    #[test]
    fn current_records_are_strict_and_legacy_records_require_migration() {
        let a = RunRecord {
            schema: RUN_RECORD_SCHEMA,
            record_id: RECORD_A.into(),
            ts_ms: 1,
            subject: test_subject("j"),
            kind: RunKind::Apply,
            done: 3,
            skipped: 0,
            errors: 1,
            bytes: 42,
            elapsed_ms: 100,
            cancelled: false,
            artifacts: RunArtifacts::Directory {
                run_id: "20260101-000000-j-apply".into(),
            },
            warnings: 2,
            ops_found: None,
            finished: true,
        };
        let s = serde_json::to_string(&a).unwrap();
        let b: RunRecord = serde_json::from_str(&s).unwrap();
        assert_eq!((b.done, b.errors, b.warnings), (3, 1, 2));
        let old = r#"{"ts_ms":9,"job":"j","kind":"apply","done":1,"skipped":0,"errors":0,
            "bytes":0,"elapsed_ms":5,"cancelled":false,"detail":"9-j.jsonl"}"#;
        assert!(serde_json::from_str::<RunRecord>(old).is_err());
        let legacy = serde_json::from_str::<LegacyRunRecord>(old).unwrap();
        let migrated =
            migrate_legacy_record(legacy, legacy_record_id(old, "0"), &HashMap::new()).unwrap();
        assert!(matches!(
            migrated.subject.binding,
            RunJobBinding::LegacyUnbound
        ));
        assert!(matches!(
            migrated.artifacts,
            RunArtifacts::LegacyFile { ref file_name } if file_name == "9-j.jsonl"
        ));
        assert!(migrated.finished);
    }

    #[test]
    fn legacy_index_and_interrupted_summary_migrate_once_with_immutable_sources() {
        let root_path = std::env::temp_dir().join(format!(
            "syncdash-runlog-migration-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root_path);
        let run_id = "20260101-000000-job-apply";
        std::fs::create_dir_all(root_path.join(run_id)).unwrap();
        let legacy_index = format!(
            "{{\"ts_ms\":9,\"job\":\"job\",\"kind\":\"apply\",\"done\":1,\"skipped\":0,\"errors\":0,\"bytes\":0,\"elapsed_ms\":5,\"cancelled\":false,\"run_id\":\"{run_id}\",\"finished\":true}}\n"
        );
        let legacy_summary = format!(
            "{{\"ts_ms\":9,\"job\":\"job\",\"kind\":\"apply\",\"done\":0,\"skipped\":1,\"errors\":0,\"bytes\":0,\"elapsed_ms\":0,\"cancelled\":false,\"run_id\":\"{run_id}\",\"finished\":false}}"
        );
        std::fs::write(root_path.join(INDEX_FILE), &legacy_index).unwrap();
        std::fs::write(root_path.join(run_id).join(SUMMARY_FILE), &legacy_summary).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        migrate_current_schema_locked(&root, &HashMap::new()).unwrap();

        assert_eq!(
            std::fs::read_to_string(root_path.join(RUNLOG_LEGACY_INDEX_FILE)).unwrap(),
            legacy_index
        );
        assert_eq!(
            std::fs::read_to_string(root_path.join(run_id).join(RUNLOG_LEGACY_SUMMARY_FILE))
                .unwrap(),
            legacy_summary
        );
        assert_eq!(
            std::fs::read_to_string(root_path.join(RUNLOG_SCHEMA_FILE)).unwrap(),
            format!("{RUN_RECORD_SCHEMA}\n")
        );
        let index_record = parse_current_record(
            std::fs::read_to_string(root_path.join(INDEX_FILE))
                .unwrap()
                .trim(),
            "migrated test index",
        )
        .unwrap();
        let summary_record = parse_current_record(
            &std::fs::read_to_string(root_path.join(run_id).join(SUMMARY_FILE)).unwrap(),
            "migrated test summary",
        )
        .unwrap();
        assert_eq!(summary_record.record_id, index_record.record_id);
        assert!(index_record.finished);
        assert!(!summary_record.finished);
        assert!(matches!(
            index_record.subject.binding,
            RunJobBinding::LegacyUnbound
        ));

        ensure_current_schema_locked(&root).unwrap();
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[test]
    fn migration_refuses_a_conflicting_backup_without_rewriting_the_index() {
        let root_path = std::env::temp_dir().join(format!(
            "syncdash-runlog-migration-conflict-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root_path);
        std::fs::create_dir_all(&root_path).unwrap();
        let legacy_index = "{\"ts_ms\":9,\"job\":\"job\",\"kind\":\"compare\",\"done\":0,\"skipped\":0,\"errors\":0,\"bytes\":0,\"elapsed_ms\":5,\"cancelled\":false,\"ops_found\":1}\n";
        std::fs::write(root_path.join(INDEX_FILE), legacy_index).unwrap();
        std::fs::write(root_path.join(RUNLOG_LEGACY_INDEX_FILE), "different").unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        let error = migrate_current_schema_locked(&root, &HashMap::new()).unwrap_err();

        assert!(error.to_string().contains("does not match its source"));
        assert_eq!(
            std::fs::read_to_string(root_path.join(INDEX_FILE)).unwrap(),
            legacy_index
        );
        assert!(!root_path.join(RUNLOG_SCHEMA_FILE).exists());
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[test]
    fn stamp_matches_known_unix_times() {
        assert_eq!(crate::foundation::time::stamp_compact(0), "19700101-000000");
        assert_eq!(
            crate::foundation::time::stamp_compact(946_684_800_000),
            "20000101-000000"
        ); // 2000-01-01T00:00:00Z
        assert_eq!(
            crate::foundation::time::stamp_compact(1_000_000_000_000),
            "20010909-014640"
        ); // the classic billionth second
        assert_eq!(
            crate::foundation::time::stamp_compact(1_709_164_800_000),
            "20240229-000000"
        ); // leap day
    }

    #[test]
    fn stamps_sort_chronologically() {
        // Directory names must sort lexicographically as they stand — the entire reason for not pulling in chrono
        let mut v = vec![
            crate::foundation::time::stamp_compact(1_000_000_000_000),
            crate::foundation::time::stamp_compact(0),
            crate::foundation::time::stamp_compact(946_684_800_000),
        ];
        v.sort();
        assert_eq!(
            v,
            vec!["19700101-000000", "20000101-000000", "20010909-014640"]
        );
    }

    #[test]
    fn path_escapes_are_refused() {
        assert!(run_identifier("../../etc/passwd").is_err());
        assert!(run_identifier("a/b").is_err());
        assert!(run_identifier("a\\b").is_err());
        assert!(run_identifier("").is_err());
        assert!(run_identifier("20260101-000000-job-apply").is_ok());
        assert!(artifact_lines("../secrets", LogArtifactKind::Run, 10)
            .unwrap_err()
            .contains("record_id"));
    }

    #[test]
    fn reveal_targets_are_validated_before_the_presentation_callback() {
        let root_path = std::env::temp_dir().join(format!(
            "syncdash-runlog-reveal-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root_path);
        let run_id = "20260101-000000-job-apply";
        std::fs::create_dir_all(root_path.join(run_id)).unwrap();
        write_current_index(&root_path, &[test_apply_record(RECORD_A, run_id, true)]);

        let root_target =
            with_validated_reveal_target_at(root_path.clone(), None, |path| Ok(path.to_path_buf()))
                .unwrap();
        assert_eq!(root_target, root_path);
        let run_target =
            with_validated_reveal_target_at(root_path.clone(), Some(RECORD_A), |path| {
                Ok(path.to_path_buf())
            })
            .unwrap();
        assert_eq!(run_target, root_path.join(run_id));

        for rejected in ["../outside", "unrelated", RECORD_B] {
            let called = std::cell::Cell::new(false);
            assert!(
                with_validated_reveal_target_at(root_path.clone(), Some(rejected), |_| {
                    called.set(true);
                    Ok(())
                },)
                .is_err()
            );
            assert!(!called.get());
        }

        let _ = std::fs::remove_dir_all(root_path);
    }

    #[cfg(unix)]
    #[test]
    fn reveal_refuses_a_symlinked_run_directory_before_the_callback() {
        use std::os::unix::fs::symlink;

        let root_path = std::env::temp_dir().join(format!(
            "syncdash-runlog-reveal-link-root-{}",
            std::process::id()
        ));
        let outside = std::env::temp_dir().join(format!(
            "syncdash-runlog-reveal-link-outside-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root_path);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root_path).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let run_id = "20260101-000000-job-apply";
        symlink(&outside, root_path.join(run_id)).unwrap();
        write_current_index(&root_path, &[test_apply_record(RECORD_A, run_id, true)]);
        let called = std::cell::Cell::new(false);

        assert!(
            with_validated_reveal_target_at(root_path.clone(), Some(RECORD_A), |_| {
                called.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(!called.get());

        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn sanitize_strips_path_chars() {
        assert!(sanitize("a b/c\\d")
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-'));
    }

    #[test]
    fn run_directories_are_unique_inside_the_same_millisecond() {
        let root = std::env::temp_dir().join(format!(
            "syncdash-run-id-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        let local_root = LocalRoot::create(root.clone()).unwrap();
        let first = create_run_dir(&local_root, 1_700_000_000_123, "job", RunKind::Apply).unwrap();
        let second = create_run_dir(&local_root, 1_700_000_000_123, "job", RunKind::Apply).unwrap();
        assert_ne!(first.0, second.0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_run_directories_cannot_redirect_history_or_artifact_reads() {
        use std::os::unix::fs::symlink;

        let root_path = std::env::temp_dir().join(format!(
            "syncdash-runlog-confined-root-{}",
            std::process::id()
        ));
        let outside = std::env::temp_dir().join(format!(
            "syncdash-runlog-confined-outside-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root_path);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root_path).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let run_id = EntryName::try_from("20260101-000000-forged-apply").unwrap();
        let forged = pending_record(
            RECORD_A,
            &test_subject("forged"),
            RunKind::Apply,
            1,
            run_id.as_str(),
            0,
        );
        std::fs::write(
            outside.join(SUMMARY_FILE),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        std::fs::write(outside.join(RUNLOG_RUN_FILE), b"outside-secret\n").unwrap();
        symlink(&outside, root_path.join(run_id.as_str())).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        assert!(artifact_lines_at(&root, &run_id, LogArtifactKind::Run, 10).is_err());
        assert!(history_merged_at(&root, None, 10).unwrap().is_empty());
        assert_eq!(
            std::fs::read(outside.join(RUNLOG_RUN_FILE)).unwrap(),
            b"outside-secret\n"
        );

        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn index_symlinks_surface_an_error_instead_of_reading_their_target() {
        use std::os::unix::fs::symlink;

        let root_path =
            std::env::temp_dir().join(format!("syncdash-runlog-index-root-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!(
            "syncdash-runlog-index-outside-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root_path);
        let _ = std::fs::remove_file(&outside);
        std::fs::create_dir_all(&root_path).unwrap();
        std::fs::write(&outside, b"{}\n").unwrap();
        symlink(&outside, root_path.join(INDEX_FILE)).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        assert!(history_at(&root, None, 10).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"{}\n");
        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn orphan_sweep_retains_unrelated_and_unverifiable_directories() {
        let root_path = std::env::temp_dir().join(format!(
            "syncdash-runlog-orphan-root-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root_path);
        std::fs::create_dir_all(root_path.join("unrelated-project")).unwrap();
        let malformed_id = "20260101-000000-000-job-apply-123-1";
        std::fs::create_dir_all(root_path.join(malformed_id)).unwrap();
        std::fs::write(root_path.join(malformed_id).join(SUMMARY_FILE), b"not-json").unwrap();
        let mismatched_id = "20260101-000000-000-job-apply-123-2";
        std::fs::create_dir_all(root_path.join(mismatched_id)).unwrap();
        let mismatch = pending_record(
            RECORD_A,
            &test_subject("job"),
            RunKind::Apply,
            1,
            malformed_id,
            0,
        );
        std::fs::write(
            root_path.join(mismatched_id).join(SUMMARY_FILE),
            serde_json::to_vec(&mismatch).unwrap(),
        )
        .unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();
        let live = std::collections::HashSet::new();

        sweep_orphans(
            &root,
            &live,
            crate::foundation::time::now_ms() as i64 + 1_000,
        )
        .unwrap();

        assert!(root_path.join("unrelated-project").is_dir());
        assert!(root_path.join(malformed_id).is_dir());
        assert!(root_path.join(mismatched_id).is_dir());
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[cfg(unix)]
    #[test]
    fn size_measurement_failure_aborts_before_any_retention_delete() {
        use std::os::unix::fs::symlink;

        let root_path =
            std::env::temp_dir().join(format!("syncdash-runlog-size-root-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!(
            "syncdash-runlog-size-outside-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root_path);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root_path).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let first_id = "20200101-000000-000-job-apply-123-1";
        let redirected_id = "20200101-000000-000-job-apply-123-2";
        std::fs::create_dir_all(root_path.join(first_id)).unwrap();
        std::fs::write(root_path.join(first_id).join(RUNLOG_RUN_FILE), b"keep").unwrap();
        std::fs::write(outside.join("sentinel"), b"outside").unwrap();
        symlink(&outside, root_path.join(redirected_id)).unwrap();
        let records = [
            pending_record(
                RECORD_A,
                &test_subject("job"),
                RunKind::Apply,
                1,
                first_id,
                0,
            ),
            pending_record(
                RECORD_B,
                &test_subject("job"),
                RunKind::Apply,
                1,
                redirected_id,
                0,
            ),
        ];
        let index = records
            .iter()
            .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
            .collect::<String>();
        std::fs::write(root_path.join(INDEX_FILE), index).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        assert!(prune_at(&root, 1, 1).is_err());
        assert_eq!(
            std::fs::read(root_path.join(first_id).join(RUNLOG_RUN_FILE)).unwrap(),
            b"keep"
        );
        assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_dir_all(outside);
    }
}
