//! Descriptor-relative path construction and untrusted identifier parsing.

use std::path::PathBuf;

use crate::foundation::names::{
    RUNLOG_INDEX_FILE as INDEX_FILE, RUNLOG_INDEX_LOCK_FILE, RUNLOG_LEGACY_INDEX_FILE,
    RUNLOG_LEGACY_SUMMARY_FILE, RUNLOG_SCHEMA_FILE,
};
use crate::foundation::path::{EntryName, RootRelativeDir, RootRelativePath};

use super::model::LogArtifactKind;

pub fn logs_dir() -> PathBuf {
    crate::store::settings::load().resolved_log_dir()
}

pub(super) fn sanitize(name: &str) -> String {
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

pub(super) fn run_identifier(value: &str) -> Result<EntryName, String> {
    EntryName::try_from(value).map_err(|error| format!("Invalid run identifier: {error}"))
}

pub(super) fn reveal_run_identifier(value: &str) -> Result<EntryName, String> {
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

pub(super) fn root_directory() -> RootRelativeDir {
    RootRelativeDir::try_from("").expect("the empty relative directory denotes the root")
}

pub(super) fn index_relative_path() -> RootRelativePath {
    RootRelativePath::try_from(INDEX_FILE).expect("the run index name is a valid relative path")
}

pub(super) fn legacy_index_relative_path() -> RootRelativePath {
    RootRelativePath::try_from(RUNLOG_LEGACY_INDEX_FILE)
        .expect("the legacy run index name is a valid relative path")
}

pub(super) fn index_lock_relative_path() -> RootRelativePath {
    RootRelativePath::try_from(RUNLOG_INDEX_LOCK_FILE)
        .expect("the run-index lock name is a valid relative path")
}

pub(super) fn schema_relative_path() -> RootRelativePath {
    RootRelativePath::try_from(RUNLOG_SCHEMA_FILE)
        .expect("the run schema marker name is a valid relative path")
}

pub(super) fn artifact_relative_path(
    run_id: &EntryName,
    artifact: LogArtifactKind,
) -> RootRelativePath {
    RootRelativePath::try_from(format!("{run_id}/{}", artifact.file_name()))
        .expect("validated run and artifact names form a valid relative path")
}

pub(super) fn legacy_detail_relative_path(detail: &EntryName) -> RootRelativePath {
    RootRelativePath::try_from(detail.as_str())
        .expect("a validated detail entry is a valid relative path")
}

pub(super) fn legacy_summary_relative_path(run_id: &EntryName) -> RootRelativePath {
    RootRelativePath::try_from(format!("{run_id}/{RUNLOG_LEGACY_SUMMARY_FILE}"))
        .expect("validated run and summary names form a valid relative path")
}

pub(super) fn random_record_id() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| std::io::Error::other(format!("cannot create a run identity: {error}")))?;
    Ok(hex_bytes(&bytes))
}

pub(super) fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}
