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
pub(crate) enum LegacyPolicy {
    Accept,
    Reject,
}

/// The cache filename deliberately keeps the historical 64-bit prefix. The header carries the
/// full digest of either a normalized remote key or `localid`'s volume-plus-relative-root bytes,
/// so a copied file, replacement disk, or vanishingly unlikely short-name collision is rejected
/// before any row is trusted. Only the digest is persisted; roots and remote phrases stay private.
fn root_binding(binding: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(binding).to_hex())
}

pub(crate) fn logical_scan_state_binding(key: &str) -> Vec<u8> {
    key.to_lowercase().into_bytes()
}

fn classify_header(line: &str, kind: &str, binding: &[u8]) -> HeaderLine {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return HeaderLine::NotHeader;
    };
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(SCAN_STATE_SCHEMA) {
        return HeaderLine::NotHeader;
    }
    if value.get("version").and_then(serde_json::Value::as_u64)
        != Some(SCAN_STATE_VERSION as u64)
    {
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

/// Stream either the historical headerless JSONL rows or the current versioned form.
///
/// Invalid individual rows retain the old best-effort behavior and are skipped. A recognizable
/// header that is unsupported, malformed, duplicated, or bound to another root rejects the whole
/// file: using no cache is always safer than trusting state whose provenance is uncertain.
pub(crate) fn read_scan_state<T: serde::de::DeserializeOwned>(
    path: &Path,
    kind: &str,
    binding: &[u8],
    legacy: LegacyPolicy,
    mut consume: impl FnMut(T),
) -> bool {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return true,
    };
    let mut saw_content = false;
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else {
            return false;
        };
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
                HeaderLine::NotHeader if legacy == LegacyPolicy::Accept => {}
                HeaderLine::NotHeader => return false,
                HeaderLine::UnsupportedVersion
                | HeaderLine::Mismatched
                | HeaderLine::Malformed => return false,
            }
        } else if line.contains(SCAN_STATE_SCHEMA)
            && classify_header(line, kind, binding) != HeaderLine::NotHeader
        {
            // Keep normal rows on one serde pass. Only a line carrying the reserved schema marker
            // pays for header classification after the first record.
            return false;
        }
        saw_content = true;
        if let Ok(row) = serde_json::from_str::<T>(line) {
            consume(row);
        }
    }
    // An existing empty file is also headerless. Remote logical keys retain the historical
    // best-effort acceptance; local state cannot prove which physical disk created it.
    saw_content || legacy == LegacyPolicy::Accept
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

fn scan_state_rewrite_allowed(path: &Path, kind: &str, binding: &[u8]) -> bool {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
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
        return classify_header(line, kind, binding) != HeaderLine::UnsupportedVersion;
    }
    true
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
    if !scan_state_rewrite_allowed(destination, kind, binding) {
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

/// Where a per-root cache table lives, given the root's identity and a file extension.
///
/// The key is hashed rather than sanitized: a root phrase can hold a drive letter, a UNC prefix,
/// a URL with credentials, or CJK — none of which survives being turned into a filename, and all
/// of which must still map to one stable file. Lowercased first, so a Windows root spelled two
/// ways does not end up with two caches that each see half the tree as new.
pub(crate) fn cache_file(key: &str, ext: &str) -> PathBuf {
    let h = blake3::hash(key.to_lowercase().as_bytes());
    crate::foundation::dirs::data_dir().join("hashcache").join(format!("{}.{ext}", &h.to_hex()[..16]))
}

pub(crate) fn rewrite_atomic(
    destination: &std::path::Path,
    write: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let directory = destination.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "cache destination has no parent")
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
            Err(std::io::Error::new(std::io::ErrorKind::Other, "injected failure"))
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
            Err(std::io::Error::new(std::io::ErrorKind::Other, "injected failure"))
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
        assert!(!super::read_scan_state(
            &destination,
            "test",
            b"root",
            super::LegacyPolicy::Accept,
            |row: TestRow| {
                rows.push(row.value);
            },
        ));
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
