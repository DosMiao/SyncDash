//! Locked, fail-closed migration from supported legacy indexes and summaries.

use std::collections::HashMap;
use std::io::Write;

use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;

use super::codec::{
    invalid_data, legacy_kind, legacy_record_id, parse_current_record, validate_record,
};
use super::model::{
    LegacyRunRecord, LogArtifactKind, RunArtifacts, RunJobBinding, RunRecord, RunSubject,
    RUN_RECORD_SCHEMA,
};
use super::paths::{
    artifact_relative_path, index_lock_relative_path, index_relative_path,
    legacy_index_relative_path, legacy_summary_relative_path, reveal_run_identifier,
    root_directory, schema_relative_path,
};
use super::storage::read_optional_text;

fn registered_job_bindings() -> std::io::Result<HashMap<String, String>> {
    crate::job::load_all().map(|jobs| {
        jobs.into_iter()
            .filter(|(_, job)| job.targets.len() == 1)
            .map(|(name, job)| (name, job.job_id))
            .collect()
    })
}

pub(super) fn migrate_legacy_record(
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
    for entry in root.read_directory(&root_directory())?.entries {
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

pub(super) fn ensure_current_schema_locked(root: &LocalRoot) -> std::io::Result<()> {
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

pub(super) fn migrate_current_schema_locked(
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

pub(super) fn with_index_lock<T>(
    root: &LocalRoot,
    operation: impl FnOnce(&LocalRoot) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let lock = root.open_lock_file(&index_lock_relative_path())?;
    lock.lock()?;
    ensure_current_schema_locked(root)?;
    operation(root)
}
