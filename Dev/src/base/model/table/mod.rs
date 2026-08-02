//! Strict JSONL observations exchanged by scan, compare, peer, and archive flows.

use crate::model::digest::Blake3Digest;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, Write};
use std::path::Path;

use crate::foundation::path::RootRelativePath;

pub(crate) mod migrate;

pub const TABLE_SCHEMA: u32 = 2;
pub const ARCHIVE_GENERATIONS: usize = 3;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    Snapshot,
    Archive,
}

impl TableKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Archive => "archive",
        }
    }
}

impl fmt::Display for TableKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TableEvidence {
    None,
    Sampled,
    Full,
}

impl TableEvidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sampled => "sampled",
            Self::Full => "full",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservedMedium {
    FixedDisk,
    RemovableDisk,
    NetworkShare,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservedNameRules {
    Windows,
    Posix,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VfsNote {
    pub protocol: String,
    pub display_root: String,
    pub mtime_precision_ms: u32,
    pub medium: ObservedMedium,
    pub name_rules: ObservedNameRules,
    pub degraded: Vec<String>,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TableHeader {
    pub schema: u32,
    pub kind: TableKind,
    pub root: String,
    pub host: String,
    pub os: String,
    pub scanned_at_ms: u64,
    pub duration_ms: u64,
    pub entry_count: u64,
    pub evidence: TableEvidence,
    pub excluded_dirs: u64,
    pub excluded_files: u64,
    pub walk_errors: u64,
    pub walk_err_samples: Vec<String>,
    pub icloud_stubs: u64,
    pub icloud_stub_samples: Vec<String>,
    pub skipped_symlinks: u64,
    pub dataless_files: u64,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub vfs: Option<VfsNote>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileIdentityObservation {
    SizeAndMtime,
    SampledBlake3 { digest: Blake3Digest },
    FullBlake3 { digest: Blake3Digest },
    Unreadable,
}

impl FileIdentityObservation {
    pub fn digest(&self) -> Option<&Blake3Digest> {
        match self {
            Self::SampledBlake3 { digest } | Self::FullBlake3 { digest } => Some(digest),
            Self::SizeAndMtime | Self::Unreadable => None,
        }
    }

    pub fn is_unreadable(&self) -> bool {
        matches!(self, Self::Unreadable)
    }

    pub fn plan_hash(&self) -> Option<String> {
        match self {
            Self::SampledBlake3 { digest } => Some(format!("~{digest}")),
            Self::FullBlake3 { digest } => Some(digest.as_str().to_owned()),
            Self::SizeAndMtime | Self::Unreadable => None,
        }
    }

    fn is_digest(&self) -> bool {
        matches!(self, Self::SampledBlake3 { .. } | Self::FullBlake3 { .. })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObservedFile {
    pub path: RootRelativePath,
    pub size: u64,
    pub mtime_ms: i64,
    pub identity: FileIdentityObservation,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub file_system_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub mode: Option<u32>,
    pub previous_identities: Vec<FileIdentityObservation>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObservedDirectory {
    pub path: RootRelativePath,
    pub mtime_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObservedSymlink {
    pub path: RootRelativePath,
    pub mtime_ms: i64,
    pub target: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "observation",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ObservedEntry {
    File(ObservedFile),
    Directory(ObservedDirectory),
    Symlink(ObservedSymlink),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedEntryKind {
    File,
    Directory,
    Symlink,
}

impl ObservedEntry {
    pub fn path(&self) -> &RootRelativePath {
        match self {
            Self::File(file) => &file.path,
            Self::Directory(directory) => &directory.path,
            Self::Symlink(symlink) => &symlink.path,
        }
    }

    pub fn kind(&self) -> ObservedEntryKind {
        match self {
            Self::File(_) => ObservedEntryKind::File,
            Self::Directory(_) => ObservedEntryKind::Directory,
            Self::Symlink(_) => ObservedEntryKind::Symlink,
        }
    }

    pub fn mtime_ms(&self) -> i64 {
        match self {
            Self::File(file) => file.mtime_ms,
            Self::Directory(directory) => directory.mtime_ms,
            Self::Symlink(symlink) => symlink.mtime_ms,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            Self::File(file) => file.size,
            Self::Directory(_) | Self::Symlink(_) => 0,
        }
    }

    pub fn as_file(&self) -> Option<&ObservedFile> {
        match self {
            Self::File(file) => Some(file),
            Self::Directory(_) | Self::Symlink(_) => None,
        }
    }

    pub fn as_file_mut(&mut self) -> Option<&mut ObservedFile> {
        match self {
            Self::File(file) => Some(file),
            Self::Directory(_) | Self::Symlink(_) => None,
        }
    }

    pub fn as_symlink(&self) -> Option<&ObservedSymlink> {
        match self {
            Self::Symlink(symlink) => Some(symlink),
            Self::File(_) | Self::Directory(_) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TableArtifact {
    pub header: TableHeader,
    pub entries: Vec<ObservedEntry>,
}

impl TableArtifact {
    pub fn new(header: TableHeader, entries: Vec<ObservedEntry>) -> std::io::Result<Self> {
        let table = Self { header, entries };
        table.validate()?;
        Ok(table)
    }

    pub fn write_to(&self, writer: &mut dyn Write) -> std::io::Result<()> {
        self.validate()?;
        serde_json::to_writer(&mut *writer, &self.header).map_err(std::io::Error::other)?;
        writer.write_all(b"\n")?;
        for entry in &self.entries {
            serde_json::to_writer(&mut *writer, entry).map_err(std::io::Error::other)?;
            writer.write_all(b"\n")?;
        }
        Ok(())
    }

    pub fn read_snapshot(reader: impl BufRead) -> std::io::Result<Self> {
        Self::read_exact(reader, TableKind::Snapshot)
    }

    pub fn read_archive(reader: impl BufRead) -> std::io::Result<Self> {
        Self::read_exact(reader, TableKind::Archive)
    }

    pub fn load_snapshot(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::read_snapshot(std::io::BufReader::new(file))
    }

    fn read_exact(reader: impl BufRead, expected_kind: TableKind) -> std::io::Result<Self> {
        let mut lines = reader.lines();
        let header_line = lines.next().ok_or_else(|| table_error("empty table"))??;
        if header_line.trim().is_empty() {
            return Err(table_error("the table header line is empty"));
        }
        let marker: serde_json::Value = serde_json::from_str(&header_line)
            .map_err(|error| table_error(format!("invalid table header JSON: {error}")))?;
        let observed_schema = marker.get("schema").and_then(serde_json::Value::as_u64);
        if observed_schema != Some(TABLE_SCHEMA as u64) {
            let found = observed_schema
                .map(|schema| schema.to_string())
                .unwrap_or_else(|| "missing or non-integer".to_string());
            let remedy = if expected_kind == TableKind::Snapshot {
                "rebuild it with a fresh scan"
            } else {
                "run the archive migration"
            };
            return Err(table_error(format!(
                "{} table schema {found} is unsupported; {remedy} (expected {TABLE_SCHEMA})",
                expected_kind.as_str()
            )));
        }
        let header: TableHeader = serde_json::from_str(&header_line)
            .map_err(|error| table_error(format!("invalid table header: {error}")))?;
        if header.kind != expected_kind {
            return Err(table_error(format!(
                "expected a {} table, found {}",
                expected_kind, header.kind
            )));
        }
        let mut entries = Vec::new();
        for (index, line) in lines.enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                return Err(table_error(format!(
                    "table entry line {} is empty",
                    index + 2
                )));
            }
            let entry = serde_json::from_str::<ObservedEntry>(&line).map_err(|error| {
                table_error(format!(
                    "invalid table entry on line {}: {error}",
                    index + 2
                ))
            })?;
            entries.push(entry);
        }
        Self::new(header, entries)
    }

    pub fn validate(&self) -> std::io::Result<()> {
        if self.header.schema != TABLE_SCHEMA {
            return Err(table_error(format!(
                "{} table schema {} is unsupported (expected {TABLE_SCHEMA})",
                self.header.kind, self.header.schema
            )));
        }
        if self.header.entry_count != self.entries.len() as u64 {
            return Err(table_error(format!(
                "table header declares {} entries but contains {}",
                self.header.entry_count,
                self.entries.len()
            )));
        }
        validate_samples(
            "walk error",
            self.header.walk_errors,
            &self.header.walk_err_samples,
        )?;
        validate_samples(
            "iCloud stub",
            self.header.icloud_stubs,
            &self.header.icloud_stub_samples,
        )?;
        if let Some(vfs) = &self.header.vfs {
            if vfs.protocol.is_empty() || vfs.display_root.is_empty() {
                return Err(table_error(
                    "a VFS observation requires non-empty protocol and display_root",
                ));
            }
        }
        for pair in self.entries.windows(2) {
            if pair[0].path().as_str() >= pair[1].path().as_str() {
                return Err(table_error(format!(
                    "table paths must be unique and strictly sorted: {:?} then {:?}",
                    pair[0].path().as_str(),
                    pair[1].path().as_str()
                )));
            }
        }
        for entry in &self.entries {
            let Some(file) = entry.as_file() else {
                if let Some(symlink) = entry.as_symlink() {
                    if symlink.target.is_empty() || symlink.target.contains('\0') {
                        return Err(table_error(format!(
                            "symlink {:?} has an invalid empty or NUL target",
                            symlink.path.as_str()
                        )));
                    }
                }
                continue;
            };
            validate_current_identity(self.header.evidence, file)?;
            if file.previous_identities.len() > ARCHIVE_GENERATIONS {
                return Err(table_error(format!(
                    "file {:?} carries {} historic identities; the maximum is {ARCHIVE_GENERATIONS}",
                    file.path.as_str(),
                    file.previous_identities.len()
                )));
            }
            if self.header.kind == TableKind::Snapshot && !file.previous_identities.is_empty() {
                return Err(table_error(format!(
                    "snapshot entry {:?} contains archive history",
                    file.path.as_str()
                )));
            }
            if file
                .previous_identities
                .iter()
                .any(|identity| !identity.is_digest())
            {
                return Err(table_error(format!(
                    "archive history for {:?} contains a non-digest observation",
                    file.path.as_str()
                )));
            }
            if file.mode.is_some_and(|mode| mode > 0o7777) {
                return Err(table_error(format!(
                    "file {:?} carries invalid permission bits {:#o}",
                    file.path.as_str(),
                    file.mode.unwrap_or_default()
                )));
            }
        }
        Ok(())
    }
}

fn validate_samples(label: &str, count: u64, samples: &[String]) -> std::io::Result<()> {
    if samples.len() > 5 || samples.len() as u64 > count {
        return Err(table_error(format!(
            "{label} samples are inconsistent with their count"
        )));
    }
    Ok(())
}

fn validate_current_identity(evidence: TableEvidence, file: &ObservedFile) -> std::io::Result<()> {
    let valid = match evidence {
        TableEvidence::None => matches!(file.identity, FileIdentityObservation::SizeAndMtime),
        TableEvidence::Sampled => matches!(
            file.identity,
            FileIdentityObservation::SampledBlake3 { .. }
                | FileIdentityObservation::FullBlake3 { .. }
                | FileIdentityObservation::Unreadable
        ),
        TableEvidence::Full => matches!(
            file.identity,
            FileIdentityObservation::FullBlake3 { .. } | FileIdentityObservation::Unreadable
        ),
    };
    if valid {
        Ok(())
    } else {
        Err(table_error(format!(
            "file {:?} has identity {:?}, which is invalid for {} evidence",
            file.path.as_str(),
            file.identity,
            evidence.as_str()
        )))
    }
}

fn table_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

pub fn roll_generations(fresh: &mut [ObservedEntry], old: &[ObservedEntry]) {
    let previous: HashMap<&str, &ObservedFile> = old
        .iter()
        .filter_map(ObservedEntry::as_file)
        .map(|file| (file.path.as_str(), file))
        .collect();
    for fresh_file in fresh.iter_mut().filter_map(ObservedEntry::as_file_mut) {
        let Some(old_file) = previous.get(fresh_file.path.as_str()) else {
            continue;
        };
        if old_file.identity.is_digest() && old_file.identity == fresh_file.identity {
            fresh_file.previous_identities = old_file.previous_identities.clone();
            continue;
        }
        let mut history = Vec::with_capacity(ARCHIVE_GENERATIONS);
        if old_file.identity.is_digest() {
            history.push(old_file.identity.clone());
        }
        history.extend(old_file.previous_identities.iter().cloned());
        history.truncate(ARCHIVE_GENERATIONS);
        fresh_file.previous_identities = history;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(label: &str) -> Blake3Digest {
        Blake3Digest::hash_bytes(label.as_bytes())
    }

    fn file(path: &str, identity: FileIdentityObservation) -> ObservedEntry {
        ObservedEntry::File(ObservedFile {
            path: RootRelativePath::try_from(path).unwrap(),
            size: 10,
            mtime_ms: 1_700_000_000_000,
            identity,
            file_system_id: None,
            mode: None,
            previous_identities: Vec::new(),
        })
    }

    fn header(kind: TableKind, entries: usize) -> TableHeader {
        TableHeader {
            schema: TABLE_SCHEMA,
            kind,
            root: "/data".into(),
            host: "host".into(),
            os: "linux".into(),
            scanned_at_ms: 1_700_000_000_000,
            duration_ms: 12,
            entry_count: entries as u64,
            evidence: TableEvidence::Full,
            excluded_dirs: 0,
            excluded_files: 0,
            walk_errors: 0,
            walk_err_samples: Vec::new(),
            icloud_stubs: 0,
            icloud_stub_samples: Vec::new(),
            skipped_symlinks: 0,
            dataless_files: 0,
            vfs: None,
        }
    }

    #[test]
    fn current_table_round_trip_is_exact() {
        let table = TableArtifact::new(
            header(TableKind::Snapshot, 2),
            vec![
                file(
                    "a.txt",
                    FileIdentityObservation::FullBlake3 {
                        digest: digest("a"),
                    },
                ),
                file(
                    "b.txt",
                    FileIdentityObservation::FullBlake3 {
                        digest: digest("b"),
                    },
                ),
            ],
        )
        .unwrap();
        let mut bytes = Vec::new();
        table.write_to(&mut bytes).unwrap();
        let decoded = TableArtifact::read_snapshot(std::io::BufReader::new(bytes.as_slice()))
            .expect("strict v2 table");
        assert_eq!(decoded.header, table.header);
        assert_eq!(decoded.entries, table.entries);
    }

    #[test]
    fn unknown_or_missing_fields_are_rejected() {
        let header = serde_json::to_value(header(TableKind::Snapshot, 0)).unwrap();
        let mut unknown = header.clone();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("surprise".into(), serde_json::json!(true));
        let error = TableArtifact::read_snapshot(std::io::BufReader::new(
            format!("{}\n", serde_json::to_string(&unknown).unwrap()).as_bytes(),
        ))
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));

        let mut missing = header;
        missing.as_object_mut().unwrap().remove("vfs");
        let error = TableArtifact::read_snapshot(std::io::BufReader::new(
            format!("{}\n", serde_json::to_string(&missing).unwrap()).as_bytes(),
        ))
        .unwrap_err();
        assert!(error.to_string().contains("missing field `vfs`"));
    }

    #[test]
    fn noncurrent_snapshot_requires_a_rebuild() {
        let bytes = b"{\"schema\":1,\"kind\":\"snapshot\"}\n";
        let error =
            TableArtifact::read_snapshot(std::io::BufReader::new(bytes.as_slice())).unwrap_err();
        assert!(error.to_string().contains("rebuild it with a fresh scan"));
    }

    #[test]
    fn digest_accepts_only_canonical_blake3_text() {
        let canonical = digest("canonical").into_string();
        assert!(Blake3Digest::try_from(canonical.as_str()).is_ok());
        assert!(Blake3Digest::try_from(canonical.to_uppercase().as_str()).is_err());
        assert!(Blake3Digest::try_from("abc").is_err());
        assert!(Blake3Digest::try_from(format!("~{canonical}")).is_err());
    }

    #[test]
    fn generation_history_is_typed_and_bounded() {
        let mut fresh = vec![file(
            "file.bin",
            FileIdentityObservation::FullBlake3 {
                digest: digest("v4"),
            },
        )];
        let mut old = file(
            "file.bin",
            FileIdentityObservation::FullBlake3 {
                digest: digest("v3"),
            },
        );
        old.as_file_mut().unwrap().previous_identities = vec![
            FileIdentityObservation::FullBlake3 {
                digest: digest("v2"),
            },
            FileIdentityObservation::FullBlake3 {
                digest: digest("v1"),
            },
        ];
        roll_generations(&mut fresh, &[old]);
        let history = &fresh[0].as_file().unwrap().previous_identities;
        assert_eq!(history.len(), ARCHIVE_GENERATIONS);
        assert_eq!(history[0].digest(), Some(&digest("v3")));
    }
}
