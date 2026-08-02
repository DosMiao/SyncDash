//! Versioned, root-bound JSONL state shared by scanner acceleration tables.
//!
//! This façade owns the format state machine. Binding derivation, file placement, atomic
//! publication, and best-effort reporting live in independent leaves because they change for
//! different reasons.

pub(crate) mod binding;
pub(crate) mod location;
pub(crate) mod reporting;

#[cfg(test)]
mod tests;

use std::io::BufRead;
use std::path::Path;

const SCHEMA: &str = "syncdash.scan-state";
const VERSION: u32 = 1;

#[derive(serde::Deserialize, serde::Serialize)]
struct Header {
    schema: String,
    version: u32,
    kind: String,
    root_binding: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeaderLine {
    NotHeader,
    Current,
    UnsupportedVersion,
    Mismatched,
    Malformed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadStatus {
    Missing,
    Accepted,
    Rejected,
}

fn classify_header(line: &str, kind: &str, binding_bytes: &[u8]) -> HeaderLine {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return HeaderLine::NotHeader;
    };
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(SCHEMA) {
        return HeaderLine::NotHeader;
    }
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(VERSION as u64) {
        return HeaderLine::UnsupportedVersion;
    }
    let Ok(header) = serde_json::from_value::<Header>(value) else {
        return HeaderLine::Malformed;
    };
    if header.kind != kind || header.root_binding != binding::digest(binding_bytes) {
        return HeaderLine::Mismatched;
    }
    HeaderLine::Current
}

/// Stream a current, correctly bound generation.
///
/// Invalid rows are skipped. A malformed, duplicated, future, or incorrectly bound header rejects
/// the complete file because an absent cache is safer than state of uncertain provenance.
pub(crate) fn read<T: serde::de::DeserializeOwned>(
    path: &Path,
    kind: &str,
    binding_bytes: &[u8],
    mut consume: impl FnMut(T),
) -> std::io::Result<ReadStatus> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReadStatus::Missing);
        }
        Err(error) => return Err(error),
    };
    let mut saw_content = false;
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !saw_content {
            match classify_header(line, kind, binding_bytes) {
                HeaderLine::Current => {
                    saw_content = true;
                    continue;
                }
                HeaderLine::NotHeader
                | HeaderLine::UnsupportedVersion
                | HeaderLine::Mismatched
                | HeaderLine::Malformed => return Ok(ReadStatus::Rejected),
            }
        } else if line.contains(SCHEMA)
            && classify_header(line, kind, binding_bytes) != HeaderLine::NotHeader
        {
            return Ok(ReadStatus::Rejected);
        }
        saw_content = true;
        if let Ok(row) = serde_json::from_str::<T>(line) {
            consume(row);
        }
    }
    Ok(if saw_content {
        ReadStatus::Accepted
    } else {
        ReadStatus::Rejected
    })
}

/// Whether a successful scan may replace an untrusted generation with a bound one.
pub(crate) fn needs_rebuild(path: &Path, kind: &str, binding_bytes: &[u8]) -> bool {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else {
            return false;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        return matches!(
            classify_header(line, kind, binding_bytes),
            HeaderLine::NotHeader | HeaderLine::Mismatched | HeaderLine::Malformed
        );
    }
    true
}

fn rewrite_allowed(path: &Path, kind: &str, binding_bytes: &[u8]) -> std::io::Result<bool> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        return Ok(classify_header(line, kind, binding_bytes) != HeaderLine::UnsupportedVersion);
    }
    Ok(true)
}

/// Publish a complete current generation without downgrading a future version.
pub(crate) fn rewrite(
    destination: &Path,
    kind: &str,
    binding_bytes: &[u8],
    write_rows: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
) -> std::io::Result<bool> {
    if !rewrite_allowed(destination, kind, binding_bytes)? {
        return Ok(false);
    }
    super::atomic::rewrite(destination, |writer| {
        let header = Header {
            schema: SCHEMA.into(),
            version: VERSION,
            kind: kind.into(),
            root_binding: binding::digest(binding_bytes),
        };
        serde_json::to_writer(&mut *writer, &header).map_err(std::io::Error::other)?;
        writer.write_all(b"\n")?;
        write_rows(writer)
    })?;
    Ok(true)
}
