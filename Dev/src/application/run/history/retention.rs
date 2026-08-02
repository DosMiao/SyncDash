//! Two-axis retention and conservative orphan cleanup.

use std::io::Write;

use crate::foundation::path::{EntryName, RootRelativeDir};
use crate::fs::local_root::LocalRoot;

use super::migration::with_index_lock;
use super::model::{LogArtifactKind, RunArtifacts, RunRecord};
use super::paths::{
    artifact_relative_path, index_relative_path, legacy_detail_relative_path, root_directory,
    run_identifier,
};
use super::repository::open_existing_log_root;
use super::storage::read_optional_text;

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

pub(super) fn prune_at(
    root: &LocalRoot,
    keep_days: u64,
    max_total_mb: u64,
) -> std::io::Result<u64> {
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

pub(super) fn sweep_orphans(
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
