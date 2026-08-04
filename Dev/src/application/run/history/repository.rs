//! Read-only queries and validated artifact reveal/read boundaries.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::foundation::path::{EntryName, RootRelativeDir};
use crate::fs::local_root::LocalRoot;

use super::codec::{invalid_data, parse_current_record, validate_hex_identity};
use super::migration::with_index_lock;
use super::model::{LatestRunRecord, LogArtifactKind, RunArtifacts, RunRecord};
use super::paths::{
    artifact_relative_path, index_relative_path, legacy_detail_relative_path, logs_dir,
    reveal_run_identifier, root_directory, run_identifier,
};
use super::storage::read_optional_text;

pub(super) fn open_existing_log_root() -> std::io::Result<Option<LocalRoot>> {
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

pub(super) fn with_validated_reveal_target_at<T>(
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

/// History (newest → oldest). A corrupt current record is reported instead of being hidden.
pub fn history(job: Option<&str>, limit: usize) -> std::io::Result<Vec<RunRecord>> {
    let Some(root) = open_existing_log_root()? else {
        return Ok(Vec::new());
    };
    with_index_lock(&root, |root| history_at(root, job, limit))
}

pub(super) fn history_at(
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

pub(super) fn history_merged_at(
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
        Ok(listing) => {
            for entry in listing.entries {
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

pub(super) fn artifact_lines_at(
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
