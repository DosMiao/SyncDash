//! The content-hash cache: `(path, size, mtime) -> hash`, so a rescan re-reads only what moved.
//!
//! L1 rather than inside `pipeline::scan` because it is persistent on-disk state with its own
//! lifetime, keyed by root and outliving any single run — the same reason `trash` and `version`
//! live here. `scan` is its only writer today, but nothing about the format is scan's business.
//!
//! Best-effort throughout: a cache that cannot be read or written costs speed, never correctness.
//! Every entry is re-validated against the file's current size and mtime before it is trusted.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::model::table::Entry;

/// Cache identity: for a local root the root string exactly as spelled (existing cache files keep
/// their names — pinned by a regression test); for a VFS root `Vfs::identity()`, so different
/// hosts, users or protocols never share a cache.
fn file_for_key(key: &str) -> PathBuf {
    super::cache_file(key, "jsonl")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheLine {
    path: String,
    size: u64,
    mtime_ms: i64,
    hash: String,
}

/// `path -> (size, mtime_ms, hash)`.
pub fn load_by_key(key: &str) -> HashMap<String, (u64, i64, String)> {
    let mut map = HashMap::new();
    if let Ok(f) = std::fs::File::open(file_for_key(key)) {
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            if let Ok(c) = serde_json::from_str::<CacheLine>(&line) {
                map.insert(c.path, (c.size, c.mtime_ms, c.hash));
            }
        }
    }
    map
}

pub fn load(root: &Path) -> HashMap<String, (u64, i64, String)> {
    load_by_key(&root.to_string_lossy())
}

/// Rewrite the cache from a finished snapshot. Entries without a hash are skipped rather than
/// stored empty — an unhashed entry has nothing to cache and would only have to be re-read anyway.
pub fn save_by_key(key: &str, entries: &[Entry]) {
    let file = file_for_key(key);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(f) = std::fs::File::create(&file) {
        let mut w = std::io::BufWriter::new(f);
        for e in entries {
            if let Some(h) = &e.hash {
                let c = CacheLine { path: e.path.clone(), size: e.size, mtime_ms: e.mtime_ms, hash: h.clone() };
                let _ = writeln!(w, "{}", serde_json::to_string(&c).unwrap());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::table::EntryKind;

    fn entry(path: &str, hash: Option<&str>) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::File,
            size: 42,
            mtime_ms: 1_700_000_000_000,
            hash: hash.map(String::from),
            file_id: None,
            mode: None,
            link: None,
            prev: None,
        }
    }

    #[test]
    fn a_saved_cache_reloads_what_was_hashed_and_nothing_else() {
        let key = format!("hashcache-test-{}", std::process::id());
        save_by_key(&key, &[entry("a.txt", Some("aaa")), entry("b.txt", None)]);
        let back = load_by_key(&key);
        assert_eq!(back.get("a.txt").map(|v| v.2.as_str()), Some("aaa"));
        assert!(!back.contains_key("b.txt"), "an unhashed entry has nothing worth caching");
        let _ = std::fs::remove_file(file_for_key(&key));
    }

    /// The historical key was blake3(root_string.to_lowercase())[..16] over the root exactly as
    /// spelled. LocalVfs::identity() reproduces that string, so every pre-VFS cache file must keep
    /// its name — a changed formula silently invalidates every cache on every machine.
    #[test]
    fn the_cache_key_formula_is_pinned_to_the_pre_vfs_one() {
        let root = r"D:\Some\Root";
        let expected_stem = &blake3::hash(root.to_lowercase().as_bytes()).to_hex()[..16];
        let got = file_for_key(&std::path::PathBuf::from(root).to_string_lossy());
        assert_eq!(got.file_name().unwrap().to_string_lossy(), format!("{expected_stem}.jsonl"));
    }

    #[test]
    fn an_absent_cache_is_empty_rather_than_an_error() {
        assert!(load_by_key("hashcache-definitely-never-written").is_empty());
    }
}
