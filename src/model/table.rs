//! Table format: JSONL —— the first line is a Header, every line after it is one Entry.
//! Why JSONL: it streams both ways (pipe it straight over ssh), appends incrementally, and one bad line does not ruin the whole table.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

pub const SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Header {
    pub schema: u32,
    pub kind: String, // "snapshot" | "plan" | "archive"
    pub root: String,
    pub host: String,
    pub os: String,
    pub scanned_at_ms: u64,
    pub duration_ms: u64,
    pub entry_count: u64,
    pub hashed: bool,
    /// Directories excluded by the filter (their whole subtree never entered the table) — exclusions must be visible, never silent
    #[serde(default)]
    pub excluded_dirs: u64,
    /// Files excluded by the filter
    #[serde(default)]
    pub excluded_files: u64,
    /// A VFS root's self-description (None for plain local roots): protocol, display
    /// root, mtime precision, the evidence tier this scan *actually* ran, and any
    /// declared degradations — the snapshot must say for itself how its evidence was
    /// gathered (the no-silent rule, landed on the table).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs: Option<VfsNote>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VfsNote {
    pub protocol: String,
    pub display_root: String,
    pub mtime_precision_ms: u32,
    /// "none" | "sampled" | "full" — what the scan really did (post-preflight)
    pub evidence_effective: String,
    /// "windows" | "posix" | "unknown" — the naming rules writes to this root must satisfy.
    /// `Header.os` cannot answer this for a VFS root: it carries the protocol name there, and
    /// even when it carries an OS it is the *scanning* machine's, not the root's.
    #[serde(default)]
    pub name_rules: String,
    /// Capabilities explicitly degraded for this run (human-readable lines from the preflight report)
    #[serde(default)]
    pub degraded: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    /// Path relative to root, always '/'-separated (comparable across platforms)
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// unix: dev:inode; empty on windows for now. Only corroborates same-machine moves; across machines we rely on hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    /// unix permission bits (octal mode). SMB cannot carry the exec bit, so record it here and restore it in the v0.4 pack mode.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<u32>,
    /// symlink target (recorded when symlinks="direct"; comparison is string equality on the target)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub link: Option<String>,
    /// **archive tables only**: content hashes of this path's earlier generations, newest first (P1-3).
    /// Why: if one side's current content is merely "parked on an older generation" it was not modified
    /// concurrently and must not be reported as both-changed. The archive model's cheap approximation
    /// of a vector clock (syncthing achieves the same with `PreviousBlocksHash`,
    ///  `lib/protocol/bep_fileinfo.go:200-207`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prev: Option<Vec<String>>,
}

/// How many generations of history the archive keeps. 3 covers the common "edited but never
/// refreshed the archive" case and costs only a few dozen extra bytes per entry.
pub const ARCHIVE_GENERATIONS: usize = 3;

impl Entry {
    /// Whether the current content equals **any generation this entry has recorded**
    pub fn matches_any_generation(&self, hash: &str) -> bool {
        if self.hash.as_deref() == Some(hash) {
            return true;
        }
        self.prev.as_ref().map(|v| v.iter().any(|h| h == hash)).unwrap_or(false)
    }
}

/// Roll a new archive generation: push the old archive's hash for the same path onto the `prev` chain.
/// `fresh` is the freshly scanned snapshot (rewritten in place); `old` is the previous archive.
pub fn roll_generations(fresh: &mut [Entry], old: &[Entry]) {
    use std::collections::HashMap;
    let prior: HashMap<&str, &Entry> = old.iter().map(|e| (e.path.as_str(), e)).collect();
    for e in fresh.iter_mut() {
        let Some(o) = prior.get(e.path.as_str()) else { continue };
        // Unchanged content needs no new generation — keeps the history from filling up with one repeated hash
        if o.hash.is_some() && o.hash == e.hash {
            e.prev = o.prev.clone();
            continue;
        }
        let mut chain = Vec::with_capacity(ARCHIVE_GENERATIONS);
        if let Some(h) = &o.hash {
            chain.push(h.clone());
        }
        if let Some(p) = &o.prev {
            chain.extend(p.iter().cloned());
        }
        chain.truncate(ARCHIVE_GENERATIONS);
        e.prev = if chain.is_empty() { None } else { Some(chain) };
    }
}

pub struct Snapshot {
    pub header: Header,
    pub entries: Vec<Entry>,
}

impl Snapshot {
    pub fn write_to(&self, w: &mut dyn Write) -> std::io::Result<()> {
        writeln!(w, "{}", serde_json::to_string(&self.header)?)?;
        for e in &self.entries {
            writeln!(w, "{}", serde_json::to_string(e)?)?;
        }
        Ok(())
    }

    pub fn load(path: &Path) -> std::io::Result<Snapshot> {
        let f = std::fs::File::open(path)?;
        Self::from_reader(std::io::BufReader::new(f))
    }

    /// Parse from any stream (feed the stdout of a remote ssh scan straight in)
    pub fn from_reader(r: impl BufRead) -> std::io::Result<Snapshot> {
        let mut lines = r.lines();
        let head_line = lines
            .next()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "empty table"))??;
        let header: Header = serde_json::from_str(&head_line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad header: {e}")))?;
        let mut entries = Vec::new();
        for line in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let e: Entry = serde_json::from_str(&line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad entry: {e}")))?;
            entries.push(e);
        }
        Ok(Snapshot { header, entries })
    }
}


pub fn os_name() -> String {
    std::env::consts::OS.to_string()
}

pub fn host_name() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into())
}
