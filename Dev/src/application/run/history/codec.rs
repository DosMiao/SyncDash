//! Strict current-schema parsing and legacy value decoding.

use crate::foundation::path::EntryName;

use super::model::{RunArtifacts, RunJobBinding, RunKind, RunRecord, RUN_RECORD_SCHEMA};
use super::paths::{hex_bytes, run_identifier};

pub(super) fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

pub(super) fn validate_hex_identity(value: &str, label: &str) -> std::io::Result<()> {
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

pub(super) fn validate_record(record: &RunRecord) -> std::io::Result<()> {
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

pub(super) fn parse_current_record(raw: &str, context: &str) -> std::io::Result<RunRecord> {
    let record = serde_json::from_str::<RunRecord>(raw)
        .map_err(|error| invalid_data(format!("invalid {context}: {error}")))?;
    validate_record(&record)
        .map_err(|error| invalid_data(format!("invalid {context}: {error}")))?;
    Ok(record)
}

pub(super) fn legacy_record_id(raw: &str, discriminator: &str) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(b"syncdash-run-record-v2-legacy\0");
    digest.update(discriminator.as_bytes());
    digest.update(b"\0");
    digest.update(raw.as_bytes());
    hex_bytes(&digest.finalize().as_bytes()[..16])
}

pub(super) fn legacy_kind(kind: &str) -> std::io::Result<RunKind> {
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
