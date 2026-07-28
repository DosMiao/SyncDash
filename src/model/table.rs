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
    /// Entries the walk could not read and skipped. Distinct from `excluded_*`: an exclusion is a
    /// choice the user made, this is a tree the scan could not see. Both are equally invisible to
    /// compare — a skipped entry reads as "absent on this side", which under mirror is a delete —
    /// so the count has to cross the module boundary and reach the guards, not just a log line.
    /// Only races survive to here; anything structural aborts the scan (see `scan::local`).
    #[serde(default)]
    pub walk_errors: u64,
    /// Up to five sampled walk failures, so the guard's refusal can name what it could not read
    /// instead of just counting.
    #[serde(default)]
    pub walk_err_samples: Vec<String>,
    /// `.<name>.icloud` placeholders: the file's contents are not on this disk and its real name is
    /// not in this table. Syncing one copies a few hundred bytes of plist over the real file on the
    /// other side, so this blocks rather than warns.
    #[serde(default)]
    pub icloud_stubs: u64,
    #[serde(default)]
    pub icloud_stub_samples: Vec<String>,
    /// Files evicted in place (macOS `SF_DATALESS`). Unlike a stub these still carry the true name,
    /// size and mtime, so nothing is lost — but reading one downloads it, which is worth saying
    /// before a full-rigor scan hydrates an entire library.
    #[serde(default)]
    pub dataless_files: u64,
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
    /// "fixed disk" | "removable disk" | "network share" | "unknown" — what the root sits on.
    /// It decides where preserved originals go, so a table that omits it cannot account for its
    /// own run. Empty on snapshots written before the volume probe existed.
    #[serde(default)]
    pub medium: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: u64, hash: Option<&str>) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::File,
            size,
            mtime_ms: 1_700_000_000_000,
            hash: hash.map(String::from),
            file_id: None,
            mode: None,
            link: None,
            prev: None,
        }
    }

    fn header() -> Header {
        Header {
            schema: 1,
            kind: "snapshot".into(),
            root: r"D:\data".into(),
            host: "win01".into(),
            os: "windows".into(),
            scanned_at_ms: 1_700_000_000_000,
            duration_ms: 12,
            entry_count: 0,
            hashed: true,
            excluded_dirs: 3,
            excluded_files: 4,
            walk_errors: 0,
            walk_err_samples: Vec::new(),
            icloud_stubs: 0,
            icloud_stub_samples: Vec::new(),
            dataless_files: 0,
            vfs: None,
        }
    }

    /// The snapshot is the format every stage hands to the next and the one an archive is read
    /// back from months later. A field that silently stops surviving the trip is a whole class
    /// of wrong answers, so pin the round-trip rather than the struct.
    #[test]
    fn a_snapshot_survives_write_then_read() {
        let snap = Snapshot {
            header: header(),
            entries: vec![
                entry("a/one.txt", 10, Some("abc")),
                entry("b/two.bin", 2048, None),
            ],
        };
        let mut buf: Vec<u8> = Vec::new();
        snap.write_to(&mut buf).unwrap();
        let back = Snapshot::from_reader(std::io::BufReader::new(&buf[..])).unwrap();

        assert_eq!(back.header.root, snap.header.root);
        assert_eq!(back.header.hashed, snap.header.hashed);
        assert_eq!(back.header.excluded_dirs, 3, "exclusion counts must survive — the UI reports them");
        assert_eq!(back.header.excluded_files, 4);
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.entries[0].path, "a/one.txt");
        assert_eq!(back.entries[0].hash.as_deref(), Some("abc"));
        assert_eq!(back.entries[1].hash, None, "an absent hash must stay absent, not become empty string");
    }

    /// One JSON object per line is what lets an ssh pipe, an archive and an audit share a format.
    #[test]
    fn the_table_is_one_json_object_per_line() {
        let snap = Snapshot { header: header(), entries: vec![entry("x", 1, None), entry("y", 2, None)] };
        let mut buf: Vec<u8> = Vec::new();
        snap.write_to(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "one header line plus one line per entry");
        for l in &lines {
            serde_json::from_str::<serde_json::Value>(l).expect("every line parses on its own");
        }
    }

    /// Generations are the archive's cheap stand-in for a causal clock: the newest hash is the
    /// current one and older ones ride along, newest first, capped.
    #[test]
    fn roll_generations_chains_history_newest_first_and_caps() {
        let mut fresh = vec![entry("f", 1, Some("v4"))];
        let old = vec![Entry { prev: Some(vec!["v2".into(), "v1".into()]), ..entry("f", 1, Some("v3")) }];
        roll_generations(&mut fresh, &old);
        let prev = fresh[0].prev.clone().expect("history must attach");
        assert_eq!(prev.first().map(String::as_str), Some("v3"), "the previous current hash leads");
        assert!(prev.len() <= ARCHIVE_GENERATIONS, "history is capped at {ARCHIVE_GENERATIONS}");
    }

    /// An unchanged file must not accumulate a generation per scan.
    #[test]
    fn roll_generations_does_not_grow_when_content_is_unchanged() {
        let mut fresh = vec![entry("f", 1, Some("same"))];
        let old = vec![entry("f", 1, Some("same"))];
        roll_generations(&mut fresh, &old);
        let prev = fresh[0].prev.clone().unwrap_or_default();
        assert!(!prev.contains(&"same".to_string()), "the current hash must not also sit in its own history");
    }
}
