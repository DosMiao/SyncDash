//! The content-hash cache: `(path, size, mtime) -> hash`, so a rescan re-reads only what moved.
//!
//! L1 rather than inside `pipeline::scan` because it is persistent on-disk state with its own
//! lifetime, keyed by root and outliving any single run — the same reason `trash` and `version`
//! live here. `scan` is its only writer today, but nothing about the format is scan's business.
//!
//! Best-effort throughout: a cache that cannot be read or written costs speed, never correctness.
//! Every entry is re-validated against the file's current size and mtime before it is trusted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::table::Entry;

const STATE_KIND: &str = "hashcache";

/// Cache identity: for a local root the root string exactly as spelled (existing cache files keep
/// their names — pinned by a regression test); for a VFS root `Vfs::identity()`, so different
/// hosts, users or protocols never share a cache.
fn file_for_key(key: &str) -> PathBuf {
    super::cache_file(key, "jsonl")
}

#[derive(serde::Deserialize)]
struct CacheLine {
    path: String,
    size: u64,
    mtime_ms: i64,
    hash: String,
}

#[derive(serde::Serialize)]
struct CacheLineRef<'a> {
    path: &'a str,
    size: u64,
    mtime_ms: i64,
    hash: &'a str,
}

/// `path -> (size, mtime_ms, hash)`.
fn load_bound_from_file(
    file: &Path,
    binding: &[u8],
    legacy: super::LegacyPolicy,
) -> HashMap<String, (u64, i64, String)> {
    let mut map = HashMap::new();
    let accepted = super::read_scan_state(
        file,
        STATE_KIND,
        binding,
        legacy,
        |c: CacheLine| {
            map.insert(c.path, (c.size, c.mtime_ms, c.hash));
        },
    );
    if !accepted {
        map.clear();
    }
    map
}

fn load_from_file(file: &Path, key: &str) -> HashMap<String, (u64, i64, String)> {
    load_bound_from_file(
        file,
        &super::logical_scan_state_binding(key),
        super::LegacyPolicy::Accept,
    )
}

pub fn load_by_key(key: &str) -> HashMap<String, (u64, i64, String)> {
    load_from_file(&file_for_key(key), key)
}

pub fn load(root: &Path) -> HashMap<String, (u64, i64, String)> {
    let identity = super::localid::LocalScanStateIdentity::for_root(root);
    load_local(&identity)
}

pub(crate) fn load_local(
    identity: &super::localid::LocalScanStateIdentity,
) -> HashMap<String, (u64, i64, String)> {
    load_bound_from_file(
        &file_for_key(identity.cache_key()),
        identity.binding(),
        super::LegacyPolicy::Reject,
    )
}

/// Rewrite the cache from a finished snapshot. Entries without a hash are skipped rather than
/// stored empty — an unhashed entry has nothing to cache and would only have to be re-read anyway.
fn save_bound_to_file(file: &Path, binding: &[u8], entries: &[Entry]) -> std::io::Result<bool> {
    super::rewrite_scan_state(file, STATE_KIND, binding, |writer| {
        for e in entries {
            if let Some(h) = &e.hash {
                let row = CacheLineRef {
                    path: &e.path,
                    size: e.size,
                    mtime_ms: e.mtime_ms,
                    hash: h,
                };
                serde_json::to_writer(&mut *writer, &row).map_err(std::io::Error::other)?;
                writer.write_all(b"\n")?;
            }
        }
        Ok(())
    })
}

fn save_to_file(file: &Path, key: &str, entries: &[Entry]) -> std::io::Result<bool> {
    save_bound_to_file(file, &super::logical_scan_state_binding(key), entries)
}

pub fn save_by_key(key: &str, entries: &[Entry]) {
    let _ = save_to_file(&file_for_key(key), key, entries);
}

pub(crate) fn save_local(
    identity: &super::localid::LocalScanStateIdentity,
    entries: &[Entry],
) {
    let _ = save_bound_to_file(
        &file_for_key(identity.cache_key()),
        identity.binding(),
        entries,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::table::EntryKind;

    fn temp_file(tag: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("syncdash-hashcache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory.join("state.jsonl")
    }

    fn cleanup(file: &Path) {
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    fn entry(path: &str, hash: Option<&str>) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::File,
            size: 42,
            mtime_ms: 1_700_000_000_000,
            hash: hash.map(String::from),
            hash_failed: false,
            file_id: None,
            mode: None,
            link: None,
            prev: None,
        }
    }

    #[test]
    fn a_saved_cache_reloads_what_was_hashed_and_nothing_else() {
        let file = temp_file("roundtrip");
        let key = "hashcache-test-root";
        save_to_file(&file, key, &[entry("a.txt", Some("aaa")), entry("b.txt", None)]).unwrap();
        let back = load_from_file(&file, key);
        assert_eq!(back.get("a.txt").map(|v| v.2.as_str()), Some("aaa"));
        assert!(!back.contains_key("b.txt"), "an unhashed entry has nothing worth caching");
        let text = std::fs::read_to_string(&file).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["schema"], "syncdash.scan-state");
        assert_eq!(first["version"], 1);
        assert_eq!(first["kind"], STATE_KIND);
        let binding = first["root_binding"].as_str().unwrap();
        assert_eq!(binding.len(), "blake3:".len() + 64, "the complete digest, not the filename prefix");
        assert!(!binding.contains(key), "root identities are not persisted in plaintext");
        cleanup(&file);
    }

    #[test]
    fn a_legacy_cache_is_accepted_and_the_next_save_migrates_it() {
        let file = temp_file("legacy");
        let key = "legacy-root";
        std::fs::write(
            &file,
            concat!(
                "{\"path\":\"old.txt\",\"size\":7,\"mtime_ms\":99,\"hash\":\"old-hash\"}\n",
                "a torn legacy row\n",
            ),
        )
        .unwrap();

        let legacy = load_from_file(&file, key);
        assert_eq!(legacy.get("old.txt"), Some(&(7, 99, "old-hash".into())));

        save_to_file(&file, key, &[entry("new.txt", Some("new-hash"))]).unwrap();
        let migrated = load_from_file(&file, key);
        assert!(!migrated.contains_key("old.txt"));
        assert_eq!(migrated.get("new.txt").map(|row| row.2.as_str()), Some("new-hash"));
        let text = std::fs::read_to_string(&file).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["schema"], "syncdash.scan-state");
        assert_eq!(first["version"], 1);
        cleanup(&file);
    }

    #[test]
    fn a_cache_copied_from_another_root_is_rejected_then_rebuilt() {
        let file = temp_file("binding");
        save_to_file(&file, "root-a", &[entry("a.txt", Some("aaa"))]).unwrap();
        assert!(load_from_file(&file, "root-b").is_empty());

        save_to_file(&file, "root-b", &[entry("b.txt", Some("bbb"))]).unwrap();
        let rebuilt = load_from_file(&file, "root-b");
        assert_eq!(rebuilt.get("b.txt").map(|row| row.2.as_str()), Some("bbb"));
        cleanup(&file);
    }

    #[test]
    fn remote_key_binding_keeps_its_case_folded_compatibility() {
        let file = temp_file("remote-binding");
        save_to_file(
            &file,
            "SFTP://User@Host/Share",
            &[entry("a.txt", Some("aaa"))],
        )
        .unwrap();
        let back = load_from_file(&file, "sftp://user@host/share");
        assert_eq!(back.get("a.txt").map(|row| row.2.as_str()), Some("aaa"));
        cleanup(&file);
    }

    #[test]
    fn local_binding_distinguishes_two_roots_on_the_same_volume() {
        let file = temp_file("local-binding");
        let roots = file.parent().unwrap().join("roots");
        std::fs::create_dir_all(roots.join("a")).unwrap();
        std::fs::create_dir_all(roots.join("b")).unwrap();
        let first = crate::store::localid::LocalScanStateIdentity::for_root(&roots.join("a"));
        let second = crate::store::localid::LocalScanStateIdentity::for_root(&roots.join("b"));

        save_bound_to_file(&file, first.binding(), &[entry("a.txt", Some("aaa"))]).unwrap();
        assert!(load_bound_from_file(
            &file,
            second.binding(),
            crate::store::LegacyPolicy::Reject,
        )
        .is_empty());
        assert_eq!(
            load_bound_from_file(&file, first.binding(), crate::store::LegacyPolicy::Reject)
                .get("a.txt")
                .map(|row| row.2.as_str()),
            Some("aaa")
        );
        cleanup(&file);
    }

    #[test]
    fn local_headerless_cache_is_rejected_and_a_successful_save_replaces_it() {
        let file = temp_file("local-legacy");
        let root = file.parent().unwrap().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let identity = crate::store::localid::LocalScanStateIdentity::for_root(&root);
        std::fs::write(
            &file,
            "{\"path\":\"stale.txt\",\"size\":7,\"mtime_ms\":99,\"hash\":\"stale\"}\n",
        )
        .unwrap();

        assert!(load_bound_from_file(
            &file,
            identity.binding(),
            crate::store::LegacyPolicy::Reject,
        )
        .is_empty());

        save_bound_to_file(&file, identity.binding(), &[entry("fresh.txt", Some("fresh"))])
            .unwrap();
        let rebuilt = load_bound_from_file(
            &file,
            identity.binding(),
            crate::store::LegacyPolicy::Reject,
        );
        assert!(!rebuilt.contains_key("stale.txt"));
        assert_eq!(rebuilt.get("fresh.txt").map(|row| row.2.as_str()), Some("fresh"));
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap()).unwrap()
                ["schema"],
            "syncdash.scan-state"
        );
        cleanup(&file);
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
