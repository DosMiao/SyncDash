use serde::Deserialize;
use std::io::BufRead;

use super::{
    FileIdentityObservation, ObservedDirectory, ObservedEntry, ObservedFile, ObservedMedium,
    ObservedNameRules, ObservedSymlink, TableArtifact, TableEvidence, TableHeader, TableKind,
    VfsNote, TABLE_SCHEMA,
};
use crate::foundation::path::RootRelativePath;
use crate::model::digest::Blake3Digest;

pub(crate) const LEGACY_ARCHIVE_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArchiveFormat {
    Current,
    LegacyV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyHeader {
    schema: u32,
    kind: String,
    root: String,
    host: String,
    os: String,
    scanned_at_ms: u64,
    duration_ms: u64,
    entry_count: u64,
    hashed: bool,
    excluded_dirs: Option<u64>,
    excluded_files: Option<u64>,
    walk_errors: Option<u64>,
    walk_err_samples: Option<Vec<String>>,
    icloud_stubs: Option<u64>,
    icloud_stub_samples: Option<Vec<String>>,
    skipped_symlinks: Option<u64>,
    dataless_files: Option<u64>,
    vfs: Option<LegacyVfsNote>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyVfsNote {
    protocol: String,
    display_root: String,
    mtime_precision_ms: u32,
    medium: Option<String>,
    evidence_effective: String,
    name_rules: Option<String>,
    degraded: Option<Vec<String>>,
}

#[derive(Deserialize)]
enum LegacyEntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyEntry {
    path: String,
    kind: LegacyEntryKind,
    size: u64,
    mtime_ms: i64,
    hash: Option<String>,
    hash_failed: Option<bool>,
    file_id: Option<String>,
    mode: Option<u32>,
    link: Option<String>,
    prev: Option<Vec<String>>,
}

pub(crate) fn classify_archive(reader: impl BufRead) -> std::io::Result<ArchiveFormat> {
    let mut lines = reader.lines();
    let line = lines
        .next()
        .ok_or_else(|| migration_error("archive table is empty"))??;
    let value: serde_json::Value = serde_json::from_str(&line)
        .map_err(|error| migration_error(format!("invalid archive header JSON: {error}")))?;
    let schema = value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| migration_error("archive header has no integer schema"))?;
    let kind = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| migration_error("archive header has no string kind"))?;
    if kind != "archive" {
        return Err(migration_error(format!(
            "archive path contains a {kind:?} table"
        )));
    }
    match schema {
        current if current == TABLE_SCHEMA as u64 => Ok(ArchiveFormat::Current),
        legacy if legacy == LEGACY_ARCHIVE_SCHEMA as u64 => Ok(ArchiveFormat::LegacyV1),
        unsupported => Err(migration_error(format!(
            "archive schema {unsupported} is unsupported; only v1 can migrate to v{TABLE_SCHEMA}"
        ))),
    }
}

pub(crate) fn migrate_v1_archive(reader: impl BufRead) -> std::io::Result<TableArtifact> {
    let mut lines = reader.lines();
    let header_line = lines
        .next()
        .ok_or_else(|| migration_error("archive table is empty"))??;
    if header_line.trim().is_empty() {
        return Err(migration_error("legacy archive header line is empty"));
    }
    let legacy_header: LegacyHeader = serde_json::from_str(&header_line)
        .map_err(|error| migration_error(format!("invalid legacy archive header: {error}")))?;
    if legacy_header.schema != LEGACY_ARCHIVE_SCHEMA || legacy_header.kind != "archive" {
        return Err(migration_error(
            "the migration input is not an exact schema-v1 archive",
        ));
    }

    let mut legacy_entries = Vec::new();
    for (index, line) in lines.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            return Err(migration_error(format!(
                "legacy archive entry line {} is empty",
                index + 2
            )));
        }
        let entry = serde_json::from_str::<LegacyEntry>(&line).map_err(|error| {
            migration_error(format!(
                "invalid legacy archive entry on line {}: {error}",
                index + 2
            ))
        })?;
        legacy_entries.push(entry);
    }
    if legacy_header.entry_count != legacy_entries.len() as u64 {
        return Err(migration_error(format!(
            "legacy archive declares {} entries but contains {}",
            legacy_header.entry_count,
            legacy_entries.len()
        )));
    }

    let evidence = migrate_evidence(&legacy_header, &legacy_entries)?;
    let vfs = migrate_vfs_note(legacy_header.vfs.as_ref(), &legacy_header.os)?;
    let mut entries = legacy_entries
        .into_iter()
        .map(migrate_entry)
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by(|left, right| left.path().as_str().cmp(right.path().as_str()));
    let header = TableHeader {
        schema: TABLE_SCHEMA,
        kind: TableKind::Archive,
        root: legacy_header.root,
        host: legacy_header.host,
        os: legacy_header.os,
        scanned_at_ms: legacy_header.scanned_at_ms,
        duration_ms: legacy_header.duration_ms,
        entry_count: entries.len() as u64,
        evidence,
        excluded_dirs: legacy_header.excluded_dirs.unwrap_or(0),
        excluded_files: legacy_header.excluded_files.unwrap_or(0),
        walk_errors: legacy_header.walk_errors.unwrap_or(0),
        walk_err_samples: legacy_header.walk_err_samples.unwrap_or_default(),
        icloud_stubs: legacy_header.icloud_stubs.unwrap_or(0),
        icloud_stub_samples: legacy_header.icloud_stub_samples.unwrap_or_default(),
        skipped_symlinks: legacy_header.skipped_symlinks.unwrap_or(0),
        dataless_files: legacy_header.dataless_files.unwrap_or(0),
        vfs,
    };
    TableArtifact::new(header, entries)
}

fn migrate_evidence(
    header: &LegacyHeader,
    entries: &[LegacyEntry],
) -> std::io::Result<TableEvidence> {
    let declared = header
        .vfs
        .as_ref()
        .map(|note| note.evidence_effective.as_str());
    let evidence = match declared {
        Some("none") => TableEvidence::None,
        Some("sampled") => TableEvidence::Sampled,
        Some("full") => TableEvidence::Full,
        Some(value) => {
            return Err(migration_error(format!(
                "legacy archive declares unknown evidence tier {value:?}"
            )))
        }
        None if !header.hashed => TableEvidence::None,
        None if entries.iter().any(|entry| {
            entry
                .hash
                .as_deref()
                .is_some_and(crate::model::digest::is_sampled_digest)
        }) =>
        {
            TableEvidence::Sampled
        }
        None => TableEvidence::Full,
    };
    if header.hashed != (evidence != TableEvidence::None) {
        return Err(migration_error(
            "legacy archive hashed flag contradicts its effective evidence tier",
        ));
    }
    Ok(evidence)
}

fn migrate_vfs_note(note: Option<&LegacyVfsNote>, os: &str) -> std::io::Result<Option<VfsNote>> {
    let Some(note) = note else {
        return Ok(None);
    };
    let medium = match note.medium.as_deref().unwrap_or("") {
        "fixed disk" => ObservedMedium::FixedDisk,
        "removable disk" => ObservedMedium::RemovableDisk,
        "network share" => ObservedMedium::NetworkShare,
        "" | "unknown" => ObservedMedium::Unknown,
        value => {
            return Err(migration_error(format!(
                "legacy archive declares unknown storage medium {value:?}"
            )))
        }
    };
    let name_rules = match note.name_rules.as_deref().unwrap_or("") {
        "windows" => ObservedNameRules::Windows,
        "posix" => ObservedNameRules::Posix,
        "unknown" => ObservedNameRules::Unknown,
        "" if os == "windows" => ObservedNameRules::Windows,
        "" => ObservedNameRules::Unknown,
        value => {
            return Err(migration_error(format!(
                "legacy archive declares unknown name rules {value:?}"
            )))
        }
    };
    Ok(Some(VfsNote {
        protocol: note.protocol.clone(),
        display_root: note.display_root.clone(),
        mtime_precision_ms: note.mtime_precision_ms,
        medium,
        name_rules,
        degraded: note.degraded.clone().unwrap_or_default(),
    }))
}

fn migrate_entry(entry: LegacyEntry) -> std::io::Result<ObservedEntry> {
    let path = RootRelativePath::try_from(entry.path.clone())
        .map_err(|error| migration_error(error.to_string()))?;
    let hash_failed = entry.hash_failed.unwrap_or(false);
    match entry.kind {
        LegacyEntryKind::File => {
            if entry.link.is_some() {
                return Err(migration_error(format!(
                    "legacy file {:?} carries a symlink target",
                    path.as_str()
                )));
            }
            let identity = migrate_identity(entry.hash.as_deref(), hash_failed)?;
            let previous_identities = entry
                .prev
                .unwrap_or_default()
                .into_iter()
                .map(|hash| migrate_historic_identity(&hash))
                .collect::<std::io::Result<Vec<_>>>()?;
            Ok(ObservedEntry::File(ObservedFile {
                path,
                size: entry.size,
                mtime_ms: entry.mtime_ms,
                identity,
                file_system_id: entry.file_id,
                mode: entry.mode,
                previous_identities,
            }))
        }
        LegacyEntryKind::Dir => {
            reject_non_directory_fields(&entry, &path)?;
            Ok(ObservedEntry::Directory(ObservedDirectory {
                path,
                mtime_ms: entry.mtime_ms,
            }))
        }
        LegacyEntryKind::Symlink => {
            if entry.size != 0
                || entry.hash.is_some()
                || hash_failed
                || entry.file_id.is_some()
                || entry.mode.is_some()
                || entry.prev.is_some()
            {
                return Err(migration_error(format!(
                    "legacy symlink {:?} contains file-only fields",
                    path.as_str()
                )));
            }
            let target = entry.link.ok_or_else(|| {
                migration_error(format!(
                    "legacy symlink {:?} has no recorded target",
                    path.as_str()
                ))
            })?;
            Ok(ObservedEntry::Symlink(ObservedSymlink {
                path,
                mtime_ms: entry.mtime_ms,
                target,
            }))
        }
    }
}

fn reject_non_directory_fields(
    entry: &LegacyEntry,
    path: &RootRelativePath,
) -> std::io::Result<()> {
    if entry.size != 0
        || entry.hash.is_some()
        || entry.hash_failed.unwrap_or(false)
        || entry.file_id.is_some()
        || entry.mode.is_some()
        || entry.link.is_some()
        || entry.prev.is_some()
    {
        return Err(migration_error(format!(
            "legacy directory {:?} contains file-only fields",
            path.as_str()
        )));
    }
    Ok(())
}

fn migrate_identity(
    hash: Option<&str>,
    hash_failed: bool,
) -> std::io::Result<FileIdentityObservation> {
    if hash_failed {
        if hash.is_some() {
            return Err(migration_error(
                "legacy entry records both a digest and a hash failure",
            ));
        }
        return Ok(FileIdentityObservation::Unreadable);
    }
    match hash {
        None => Ok(FileIdentityObservation::SizeAndMtime),
        Some(value) => migrate_historic_identity(value),
    }
}

fn migrate_historic_identity(value: &str) -> std::io::Result<FileIdentityObservation> {
    match value.strip_prefix(crate::model::digest::SAMPLED_PREFIX) {
        Some(digest) => Ok(FileIdentityObservation::SampledBlake3 {
            digest: Blake3Digest::try_from(digest)
                .map_err(|error| migration_error(error.to_string()))?,
        }),
        None => Ok(FileIdentityObservation::FullBlake3 {
            digest: Blake3Digest::try_from(value)
                .map_err(|error| migration_error(error.to_string()))?,
        }),
    }
}

fn migration_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(label: &str) -> String {
        Blake3Digest::hash_bytes(label.as_bytes()).into_string()
    }

    #[test]
    fn exact_v1_archive_migrates_to_typed_v2() {
        let full = digest("full");
        let previous = digest("previous");
        let input = format!(
            "{{\"schema\":1,\"kind\":\"archive\",\"root\":\"/r\",\"host\":\"h\",\"os\":\"linux\",\"scanned_at_ms\":1,\"duration_ms\":2,\"entry_count\":1,\"hashed\":true}}\n{{\"path\":\"file.txt\",\"kind\":\"File\",\"size\":4,\"mtime_ms\":3,\"hash\":\"{full}\",\"prev\":[\"{previous}\"]}}\n"
        );
        let table = migrate_v1_archive(std::io::BufReader::new(input.as_bytes())).unwrap();
        assert_eq!(table.header.schema, TABLE_SCHEMA);
        assert_eq!(table.header.kind, TableKind::Archive);
        let file = table.entries[0].as_file().unwrap();
        assert!(matches!(
            file.identity,
            FileIdentityObservation::FullBlake3 { .. }
        ));
        assert_eq!(file.previous_identities.len(), 1);
    }

    #[test]
    fn malformed_v1_combinations_block_migration() {
        let input = b"{\"schema\":1,\"kind\":\"archive\",\"root\":\"/r\",\"host\":\"h\",\"os\":\"linux\",\"scanned_at_ms\":1,\"duration_ms\":2,\"entry_count\":1,\"hashed\":true}\n{\"path\":\"link\",\"kind\":\"Symlink\",\"size\":0,\"mtime_ms\":3}\n";
        let error = migrate_v1_archive(std::io::BufReader::new(input.as_slice())).unwrap_err();
        assert!(error.to_string().contains("no recorded target"));
    }
}
