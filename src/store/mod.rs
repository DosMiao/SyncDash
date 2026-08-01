//! L1 on-disk state that outlives a single run.
//!
//! `settings` is app-level configuration, `trash` the local recycle store and its retention,
//! `version` the per-root version history, `hashcache` and `mtimefix` the two per-root tables the
//! scanner and the applier hand each other. All of them are best-effort: failing to record
//! something must never fail the sync itself.

pub mod hashcache;
pub(crate) mod localid;
pub mod migrate;
pub mod mtimefix;
pub mod settings;
pub mod trash;
pub mod version;
pub mod watch;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const SCAN_STATE_SCHEMA: &str = "syncdash.scan-state";
const SCAN_STATE_VERSION: u32 = 1;

#[derive(serde::Deserialize, serde::Serialize)]
struct ScanStateHeader {
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
pub enum ScanCoverage {
    /// The walker completed without errors. State for absent paths in the scanner-provided domain
    /// may be removed; deliberately filtered paths can still be retained individually.
    Complete,
    /// The walker skipped entries because of errors, so absence proves nothing for this scan.
    /// New observations still update state and invalidate state that they directly contradict.
    Partial,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateWriteStatus {
    Written,
    Unchanged,
    PreservedNewerVersion,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScanStateRead {
    Missing,
    Accepted,
    Rejected,
}

/// The short cache filename is only an index. The header carries a full digest of either the exact
/// canonical VFS identity or `localid`'s volume-plus-relative-root bytes, so a copied file,
/// replacement disk, or vanishingly unlikely filename collision is rejected before any row is
/// trusted. Only the digest is persisted; root identities stay private.
fn root_binding(binding: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(binding).to_hex())
}

pub(crate) fn logical_scan_state_binding(key: &str) -> Vec<u8> {
    let mut binding = b"syncdash.logical-state.v2\0".to_vec();
    binding.extend_from_slice(key.as_bytes());
    binding
}

fn classify_header(line: &str, kind: &str, binding: &[u8]) -> HeaderLine {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return HeaderLine::NotHeader;
    };
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(SCAN_STATE_SCHEMA) {
        return HeaderLine::NotHeader;
    }
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(SCAN_STATE_VERSION as u64) {
        return HeaderLine::UnsupportedVersion;
    }
    let Ok(header) = serde_json::from_value::<ScanStateHeader>(value) else {
        return HeaderLine::Malformed;
    };
    if header.kind != kind || header.root_binding != root_binding(binding) {
        return HeaderLine::Mismatched;
    }
    HeaderLine::Current
}

/// Stream the current versioned JSONL form.
///
/// Invalid individual rows retain the old best-effort behavior and are skipped. A recognizable
/// header that is unsupported, malformed, duplicated, or bound to another root rejects the whole
/// file: using no cache is always safer than trusting state whose provenance is uncertain.
pub(crate) fn read_scan_state<T: serde::de::DeserializeOwned>(
    path: &Path,
    kind: &str,
    binding: &[u8],
    mut consume: impl FnMut(T),
) -> std::io::Result<ScanStateRead> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ScanStateRead::Missing);
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
            match classify_header(line, kind, binding) {
                HeaderLine::Current => {
                    saw_content = true;
                    continue;
                }
                HeaderLine::NotHeader => return Ok(ScanStateRead::Rejected),
                HeaderLine::UnsupportedVersion | HeaderLine::Mismatched | HeaderLine::Malformed => {
                    return Ok(ScanStateRead::Rejected)
                }
            }
        } else if line.contains(SCAN_STATE_SCHEMA)
            && classify_header(line, kind, binding) != HeaderLine::NotHeader
        {
            // Keep normal rows on one serde pass. Only a line carrying the reserved schema marker
            // pays for header classification after the first record.
            return Ok(ScanStateRead::Rejected);
        }
        saw_content = true;
        if let Ok(row) = serde_json::from_str::<T>(line) {
            consume(row);
        }
    }
    Ok(if saw_content {
        ScanStateRead::Accepted
    } else {
        ScanStateRead::Rejected
    })
}

/// Whether a successful local scan should replace an untrusted generation with a bound one.
/// Missing files need no migration, and unknown future versions are deliberately preserved.
pub(crate) fn scan_state_needs_rebuild(path: &Path, kind: &str, binding: &[u8]) -> bool {
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
            classify_header(line, kind, binding),
            HeaderLine::NotHeader | HeaderLine::Mismatched | HeaderLine::Malformed
        );
    }
    true
}

fn scan_state_rewrite_allowed(path: &Path, kind: &str, binding: &[u8]) -> std::io::Result<bool> {
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
        return Ok(classify_header(line, kind, binding) != HeaderLine::UnsupportedVersion);
    }
    Ok(true)
}

/// Add the current header and replace the complete file through `rewrite_atomic`.
///
/// A legacy, malformed, or incorrectly bound file is rebuildable and may be replaced. A valid
/// header from an unknown version is left untouched so an older SyncDash cannot downgrade state
/// written by a newer one.
pub(crate) fn rewrite_scan_state(
    destination: &Path,
    kind: &str,
    binding: &[u8],
    write_rows: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
) -> std::io::Result<bool> {
    if !scan_state_rewrite_allowed(destination, kind, binding)? {
        return Ok(false);
    }
    rewrite_atomic(destination, |writer| {
        let header = ScanStateHeader {
            schema: SCAN_STATE_SCHEMA.into(),
            version: SCAN_STATE_VERSION,
            kind: kind.into(),
            root_binding: root_binding(binding),
        };
        serde_json::to_writer(&mut *writer, &header).map_err(std::io::Error::other)?;
        writer.write_all(b"\n")?;
        write_rows(writer)
    })?;
    Ok(true)
}

fn scan_state_file(binding: &[u8], ext: &str) -> PathBuf {
    let h = blake3::hash(binding);
    crate::foundation::dirs::data_dir()
        .join("hashcache")
        .join(format!("{}.{ext}", &h.to_hex()[..16]))
}

/// VFS identities have already normalized their scheme, host, and default port while deliberately
/// preserving case-sensitive user and root components. Hash those canonical bytes verbatim; the
/// binding header adds a domain marker separately so old case-folded headers cannot impersonate an
/// all-lowercase exact identity.
pub(crate) fn logical_scan_state_file(key: &str, ext: &str) -> PathBuf {
    scan_state_file(key.as_bytes(), ext)
}

/// Pre-versioned state lowercased the entire logical identity. It is only a migration candidate:
/// callers must still validate its header against the exact current binding before trusting rows.
pub(crate) fn legacy_logical_scan_state_file(key: &str, ext: &str) -> PathBuf {
    let h = blake3::hash(key.to_lowercase().as_bytes());
    crate::foundation::dirs::data_dir()
        .join("hashcache")
        .join(format!("{}.{ext}", &h.to_hex()[..16]))
}

/// Local state keeps its historical filename so a spelling-only Windows path change does not make
/// one physical root alternate between cache files. Its header is separately bound to the volume
/// and relative root by `localid`.
pub(crate) fn local_scan_state_file(key: &str, ext: &str) -> PathBuf {
    legacy_logical_scan_state_file(key, ext)
}

pub(crate) fn report_scan_state_read<T>(path: &Path, result: std::io::Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            crate::log_warn!(
                "scan-state",
                "scan state read failed for {}: {error}; this run will rebuild that acceleration data",
                path.display()
            );
            None
        }
    }
}

pub(crate) fn report_scan_state_write(
    path: &Path,
    result: std::io::Result<bool>,
) -> StateWriteStatus {
    match result {
        Ok(true) => StateWriteStatus::Written,
        Ok(false) => {
            crate::log_warn!(
                "scan-state",
                "scan state at {} was written by a newer SyncDash version and was left unchanged",
                path.display()
            );
            StateWriteStatus::PreservedNewerVersion
        }
        Err(error) => {
            crate::log_warn!(
                "scan-state",
                "scan state write failed for {}: {error}; sync results remain valid, but later scans may need to reread files",
                path.display()
            );
            StateWriteStatus::Failed
        }
    }
}

pub(crate) fn rewrite_atomic(
    destination: &std::path::Path,
    write: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let directory = destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cache destination has no parent",
        )
    })?;
    std::fs::create_dir_all(directory)?;
    let mut staged = crate::fs::staged::Staged::create(destination)?;
    {
        let mut buffered = std::io::BufWriter::new(&mut staged);
        write(&mut buffered)?;
        buffered.flush()?;
    }
    staged.seal(true)?;
    staged.commit()
}

#[cfg(test)]
mod tests {
    #[derive(Debug, serde::Deserialize, serde::Serialize)]
    struct TestRow {
        value: u32,
    }

    fn temp_file(tag: &str) -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("syncdash-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory.join("state.jsonl")
    }

    fn cleanup(file: &std::path::Path) {
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_failed_cache_rewrite_leaves_the_previous_generation_intact() {
        let destination = temp_file("atomic");
        std::fs::write(&destination, b"previous\n").unwrap();

        let result = super::rewrite_atomic(&destination, |writer| {
            writer.write_all(b"incomplete\n")?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "injected failure",
            ))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous\n");
        cleanup(&destination);
    }

    #[test]
    fn a_failed_versioned_rewrite_also_leaves_the_previous_generation_intact() {
        let destination = temp_file("versioned-atomic");
        std::fs::write(&destination, b"{\"value\":1}\n").unwrap();

        let result = super::rewrite_scan_state(&destination, "test", b"root", |writer| {
            serde_json::to_writer(&mut *writer, &TestRow { value: 2 })
                .map_err(std::io::Error::other)?;
            writer.write_all(b"\n")?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "injected failure",
            ))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"{\"value\":1}\n");
        cleanup(&destination);
    }

    #[test]
    fn an_unknown_state_version_is_neither_loaded_nor_downgraded() {
        let destination = temp_file("future");
        let original = format!(
            "{}\n{{\"value\":7}}\n",
            serde_json::json!({
                "schema": super::SCAN_STATE_SCHEMA,
                "version": super::SCAN_STATE_VERSION + 1,
                "kind": "test",
                "root_binding": super::root_binding(b"root"),
            })
        );
        std::fs::write(&destination, original.as_bytes()).unwrap();

        let mut rows = Vec::new();
        assert_eq!(
            super::read_scan_state(&destination, "test", b"root", |row: TestRow| {
                rows.push(row.value);
            })
            .unwrap(),
            super::ScanStateRead::Rejected
        );
        assert!(rows.is_empty());
        assert!(!super::scan_state_needs_rebuild(
            &destination,
            "test",
            b"root",
        ));
        assert!(!super::rewrite_scan_state(&destination, "test", b"root", |_| Ok(())).unwrap());
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), original);
        cleanup(&destination);
    }
}
