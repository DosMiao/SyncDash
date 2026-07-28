//! The mtime-correction table: `path -> (ondisk, intended)`.
//!
//! FAT/exFAT store timestamps at 2-second granularity, and some SMB servers truncate or shift the
//! value they are handed. Setting an mtime and forgetting it leaves compare leaning on a ±2s
//! tolerance, which can both miss a real change (edited inside the window) and invent one (a shift
//! wider than it) — and at `rigor = "quick"` the tolerance is the *only* criterion.
//!
//! syncthing's mtimeFS (`lib/fs/mtimefs.go:68`) stats the file straight back after writing, keeps
//! `(ondisk, virtual)` in its database and reports the virtual value from then on. Same idea, kept
//! in the machine-local state directory rather than polluting the scanned tree.
//!
//! L1 rather than inside `pipeline::scan`: `apply` writes this table and `scan` reads it, and while
//! it lived in `scan` that made `apply -> scan` the one sibling edge inside the pipeline layer.
//! Owned by `store`, both engines simply reach down.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

fn file_for_key(key: &str) -> PathBuf {
    super::cache_file(key, "mtimefix.jsonl")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MtimeFix {
    path: String,
    ondisk_ms: i64,
    intended_ms: i64,
}

/// `path -> (ondisk, intended)`.
pub fn load_by_key(key: &str) -> HashMap<String, (i64, i64)> {
    let mut map = HashMap::new();
    if let Ok(f) = std::fs::File::open(file_for_key(key)) {
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            if let Ok(c) = serde_json::from_str::<MtimeFix>(&line) {
                map.insert(c.path, (c.ondisk_ms, c.intended_ms));
            }
        }
    }
    map
}

pub fn load(root: &Path) -> HashMap<String, (i64, i64)> {
    load_by_key(&root.to_string_lossy())
}

/// Merge corrections in and rewrite wholesale, rather than appending forever.
pub fn record_by_key(key: &str, fixes: &[(String, i64, i64)]) {
    if fixes.is_empty() {
        return;
    }
    let file = file_for_key(key);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut map = load_by_key(key);
    for (p, ondisk, intended) in fixes {
        map.insert(p.clone(), (*ondisk, *intended));
    }
    if let Ok(f) = std::fs::File::create(&file) {
        let mut w = std::io::BufWriter::new(f);
        for (path, (ondisk_ms, intended_ms)) in map {
            let rec = MtimeFix { path, ondisk_ms, intended_ms };
            let _ = writeln!(w, "{}", serde_json::to_string(&rec).unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_correction_survives_and_later_writes_merge_rather_than_append() {
        let key = format!("mtimefix-test-{}", std::process::id());
        record_by_key(&key, &[("a.txt".into(), 1_000, 1_500)]);
        record_by_key(&key, &[("b.txt".into(), 2_000, 2_500)]);
        // Re-correcting a path replaces its row instead of leaving two to disagree
        record_by_key(&key, &[("a.txt".into(), 9_000, 9_500)]);

        let back = load_by_key(&key);
        assert_eq!(back.len(), 2, "one row per path, not one per write");
        assert_eq!(back.get("a.txt"), Some(&(9_000, 9_500)), "the newest correction wins");
        assert_eq!(back.get("b.txt"), Some(&(2_000, 2_500)));
        let _ = std::fs::remove_file(file_for_key(&key));
    }

    #[test]
    fn recording_nothing_does_not_create_a_file() {
        let key = format!("mtimefix-empty-{}", std::process::id());
        record_by_key(&key, &[]);
        assert!(!file_for_key(&key).exists());
    }
}
