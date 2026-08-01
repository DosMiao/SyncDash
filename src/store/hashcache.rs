//! The content-hash cache: `(path, size, mtime) -> hash`, so a rescan re-reads only what moved.
//!
//! L1 rather than inside `pipeline::scan` because it is persistent on-disk state with its own
//! lifetime, keyed by root and outliving any single run — the same reason `trash` and `version`
//! live here. `scan` is its only writer today, but nothing about the format is scan's business.
//!
//! Best-effort throughout: a cache that cannot be read or written costs speed, never correctness.
//! Every entry is re-validated against the file's current size and mtime before it is trusted.
//! A partial scan merges observations into the previous table. After an error-free scan, the
//! scanner identifies prior rows outside its configured filter so in-domain deletions can be
//! pruned without discarding deliberately excluded acceleration state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::table::Entry;

const STATE_KIND: &str = "hashcache";
pub type HashCache = HashMap<String, (u64, i64, String)>;

fn logical_file_for_key(key: &str) -> PathBuf {
    super::logical_scan_state_file(key, "jsonl")
}

fn legacy_logical_file_for_key(key: &str) -> PathBuf {
    super::legacy_logical_scan_state_file(key, "jsonl")
}

fn local_file_for_key(key: &str) -> PathBuf {
    super::local_scan_state_file(key, "jsonl")
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

fn read_bound_from_file(
    file: &Path,
    binding: &[u8],
) -> std::io::Result<(HashCache, super::ScanStateRead)> {
    let mut map = HashMap::new();
    let status = super::read_scan_state(file, STATE_KIND, binding, |c: CacheLine| {
        map.insert(c.path, (c.size, c.mtime_ms, c.hash));
    })?;
    if status != super::ScanStateRead::Accepted {
        map.clear();
    }
    Ok((map, status))
}

fn read_bound_best_effort(
    file: &Path,
    binding: &[u8],
) -> Option<(HashCache, super::ScanStateRead)> {
    super::report_scan_state_read(file, read_bound_from_file(file, binding))
}

fn load_bound_from_file(file: &Path, binding: &[u8]) -> HashCache {
    read_bound_best_effort(file, binding).map_or_else(HashMap::new, |(map, _)| map)
}

fn load_logical_from_files(primary: &Path, legacy: &Path, key: &str) -> Option<HashCache> {
    let binding = super::logical_scan_state_binding(key);
    let (map, status) = read_bound_best_effort(primary, &binding)?;
    if status != super::ScanStateRead::Missing || primary == legacy {
        return Some(map);
    }
    read_bound_best_effort(legacy, &binding).map(|(legacy_map, _)| legacy_map)
}

pub fn load_by_key(key: &str) -> HashCache {
    let primary = logical_file_for_key(key);
    let legacy = legacy_logical_file_for_key(key);
    load_logical_from_files(&primary, &legacy, key).unwrap_or_default()
}

pub fn load(root: &Path) -> HashCache {
    let identity = super::localid::LocalScanStateIdentity::for_root(root);
    load_local(&identity)
}

pub(crate) fn load_local(identity: &super::localid::LocalScanStateIdentity) -> HashCache {
    if !identity.persistent_reuse() {
        return HashMap::new();
    }
    load_bound_from_file(
        &local_file_for_key(identity.cache_key()),
        identity.binding(),
    )
}

fn rewrite_map(file: &Path, binding: &[u8], map: &HashCache) -> std::io::Result<bool> {
    let mut rows: Vec<_> = map.iter().collect();
    rows.sort_unstable_by(|left, right| left.0.cmp(right.0));
    super::rewrite_scan_state(file, STATE_KIND, binding, |writer| {
        for (path, (size, mtime_ms, hash)) in rows {
            let row = CacheLineRef {
                path,
                size: *size,
                mtime_ms: *mtime_ms,
                hash,
            };
            serde_json::to_writer(&mut *writer, &row).map_err(std::io::Error::other)?;
            writer.write_all(b"\n")?;
        }
        Ok(())
    })
}

fn merge_entries(map: &mut HashCache, entries: &[Entry]) {
    for entry in entries {
        if let Some(hash) = &entry.hash {
            map.insert(
                entry.path.clone(),
                (entry.size, entry.mtime_ms, hash.clone()),
            );
        } else {
            map.remove(&entry.path);
        }
    }
}

fn reconcile_bound_to_file(
    file: &Path,
    binding: &[u8],
    prior: Option<HashCache>,
    entries: &[Entry],
    coverage: super::ScanCoverage,
    retain_absent: &std::collections::HashSet<String>,
) -> std::io::Result<bool> {
    let mut map = if coverage == super::ScanCoverage::Partial {
        prior.unwrap_or_default()
    } else {
        prior
            .unwrap_or_default()
            .into_iter()
            .filter(|(path, _)| retain_absent.contains(path))
            .collect()
    };
    merge_entries(&mut map, entries);
    rewrite_map(file, binding, &map)
}

pub fn save_by_key(
    key: &str,
    entries: &[Entry],
    coverage: super::ScanCoverage,
    retain_absent: &std::collections::HashSet<String>,
) -> super::StateWriteStatus {
    let file = logical_file_for_key(key);
    let prior = if coverage == super::ScanCoverage::Partial || !retain_absent.is_empty() {
        let legacy = legacy_logical_file_for_key(key);
        let Some(map) = load_logical_from_files(&file, &legacy, key) else {
            return super::StateWriteStatus::Failed;
        };
        Some(map)
    } else {
        None
    };
    let result = reconcile_bound_to_file(
        &file,
        &super::logical_scan_state_binding(key),
        prior,
        entries,
        coverage,
        retain_absent,
    );
    super::report_scan_state_write(&file, result)
}

pub(crate) fn save_local(
    identity: &super::localid::LocalScanStateIdentity,
    entries: &[Entry],
    coverage: super::ScanCoverage,
    retain_absent: &std::collections::HashSet<String>,
) -> super::StateWriteStatus {
    if !identity.persistent_reuse() {
        return super::StateWriteStatus::Unchanged;
    }
    let file = local_file_for_key(identity.cache_key());
    let prior = if coverage == super::ScanCoverage::Partial || !retain_absent.is_empty() {
        let Some((map, _)) = read_bound_best_effort(&file, identity.binding()) else {
            return super::StateWriteStatus::Failed;
        };
        Some(map)
    } else {
        None
    };
    let result = reconcile_bound_to_file(
        &file,
        identity.binding(),
        prior,
        entries,
        coverage,
        retain_absent,
    );
    super::report_scan_state_write(&file, result)
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

    fn write_complete(file: &Path, binding: &[u8], entries: &[Entry]) {
        reconcile_bound_to_file(
            file,
            binding,
            None,
            entries,
            crate::store::ScanCoverage::Complete,
            &std::collections::HashSet::new(),
        )
        .unwrap();
    }

    #[test]
    fn a_saved_cache_reloads_what_was_hashed_and_nothing_else() {
        let file = temp_file("roundtrip");
        let key = "hashcache-test-root";
        let binding = crate::store::logical_scan_state_binding(key);
        write_complete(
            &file,
            &binding,
            &[entry("a.txt", Some("aaa")), entry("b.txt", None)],
        );
        let back = load_bound_from_file(&file, &binding);
        assert_eq!(back.get("a.txt").map(|v| v.2.as_str()), Some("aaa"));
        assert!(
            !back.contains_key("b.txt"),
            "an unhashed entry has nothing worth caching"
        );
        let text = std::fs::read_to_string(&file).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["schema"], "syncdash.scan-state");
        assert_eq!(first["version"], 1);
        assert_eq!(first["kind"], STATE_KIND);
        let binding = first["root_binding"].as_str().unwrap();
        assert_eq!(
            binding.len(),
            "blake3:".len() + 64,
            "the complete digest, not the filename prefix"
        );
        assert!(
            !binding.contains(key),
            "root identities are not persisted in plaintext"
        );
        cleanup(&file);
    }

    #[test]
    fn a_headerless_cache_is_rejected() {
        let file = temp_file("headerless");
        let key = "headerless-root";
        std::fs::write(
            &file,
            "{\"path\":\"old.txt\",\"size\":7,\"mtime_ms\":99,\"hash\":\"old-hash\"}\n",
        )
        .unwrap();
        assert!(
            load_bound_from_file(&file, &crate::store::logical_scan_state_binding(key)).is_empty()
        );
        cleanup(&file);
    }

    #[test]
    fn a_cache_copied_from_another_root_is_rejected_then_rebuilt() {
        let file = temp_file("binding");
        let first = crate::store::logical_scan_state_binding("root-a");
        let second = crate::store::logical_scan_state_binding("root-b");
        write_complete(&file, &first, &[entry("a.txt", Some("aaa"))]);
        assert!(load_bound_from_file(&file, &second).is_empty());

        write_complete(&file, &second, &[entry("b.txt", Some("bbb"))]);
        let rebuilt = load_bound_from_file(&file, &second);
        assert_eq!(rebuilt.get("b.txt").map(|row| row.2.as_str()), Some("bbb"));
        cleanup(&file);
    }

    #[test]
    fn logical_identity_folds_only_scheme_and_host() {
        fn identity(phrase: &str) -> String {
            match crate::fs::vfs::spec::parse(phrase) {
                crate::fs::vfs::spec::RootSpec::Remote(spec) => spec.identity(),
                other => panic!("expected VFS phrase, got {other:?}"),
            }
        }

        let canonical = identity("SFTP://User@HOST/Share");
        let same = identity("sftp://User@host:22/Share/");
        let different_user = identity("sftp://user@host/Share");
        let different_root = identity("sftp://User@host/share");
        assert_eq!(canonical, same);
        assert_eq!(
            logical_file_for_key(&canonical),
            logical_file_for_key(&same)
        );
        assert_ne!(
            logical_file_for_key(&canonical),
            logical_file_for_key(&different_user)
        );
        assert_ne!(
            logical_file_for_key(&canonical),
            logical_file_for_key(&different_root)
        );
        assert_ne!(
            crate::store::logical_scan_state_binding(&canonical),
            crate::store::logical_scan_state_binding(&different_root)
        );
    }

    #[test]
    fn legacy_filename_is_used_only_with_an_exact_bound_header() {
        let primary = temp_file("logical-primary");
        let legacy = primary.parent().unwrap().join("legacy.jsonl");
        let key = "sftp://User@host:22/Share";
        let exact = crate::store::logical_scan_state_binding(key);
        write_complete(&legacy, &exact, &[entry("a.txt", Some("exact"))]);
        let migrated = load_logical_from_files(&primary, &legacy, key).unwrap();
        assert_eq!(
            migrated.get("a.txt").map(|row| row.2.as_str()),
            Some("exact")
        );

        let folded = key.to_lowercase().into_bytes();
        write_complete(&legacy, &folded, &[entry("a.txt", Some("ambiguous"))]);
        assert!(load_logical_from_files(&primary, &legacy, key)
            .unwrap()
            .is_empty());
        cleanup(&primary);
    }

    #[test]
    fn old_folded_header_cannot_impersonate_an_all_lowercase_identity() {
        let primary = temp_file("logical-lowercase");
        let key = "sftp://user@host:22/share";
        write_complete(
            &primary,
            key.as_bytes(),
            &[entry("a.txt", Some("ambiguous"))],
        );
        assert!(load_logical_from_files(&primary, &primary, key)
            .unwrap()
            .is_empty());
        cleanup(&primary);
    }

    #[test]
    fn partial_snapshots_merge_but_complete_snapshots_drop_absent_rows() {
        let file = temp_file("coverage");
        let binding = b"coverage-root";
        write_complete(
            &file,
            binding,
            &[
                entry("visible.txt", Some("old")),
                entry("excluded.txt", Some("keep")),
            ],
        );

        let prior = load_bound_from_file(&file, binding);
        reconcile_bound_to_file(
            &file,
            binding,
            Some(prior),
            &[entry("visible.txt", Some("new"))],
            crate::store::ScanCoverage::Partial,
            &std::collections::HashSet::new(),
        )
        .unwrap();
        let partial = load_bound_from_file(&file, binding);
        assert_eq!(
            partial.get("visible.txt").map(|row| row.2.as_str()),
            Some("new")
        );
        assert_eq!(
            partial.get("excluded.txt").map(|row| row.2.as_str()),
            Some("keep")
        );

        reconcile_bound_to_file(
            &file,
            binding,
            None,
            &[entry("visible.txt", Some("new"))],
            crate::store::ScanCoverage::Complete,
            &std::collections::HashSet::new(),
        )
        .unwrap();
        let complete = load_bound_from_file(&file, binding);
        assert!(!complete.contains_key("excluded.txt"));
        cleanup(&file);
    }

    #[test]
    fn complete_snapshot_retains_only_absent_paths_outside_its_filter_domain() {
        let file = temp_file("domain");
        let binding = b"domain-root";
        write_complete(
            &file,
            binding,
            &[
                entry("deleted-in-scope.txt", Some("delete")),
                entry("excluded/keep.txt", Some("keep")),
                entry(".syncdash/state", Some("self")),
            ],
        );
        let prior = load_bound_from_file(&file, binding);
        let retain = std::collections::HashSet::from([
            "excluded/keep.txt".to_string(),
            ".syncdash/state".to_string(),
        ]);

        reconcile_bound_to_file(
            &file,
            binding,
            Some(prior),
            &[],
            crate::store::ScanCoverage::Complete,
            &retain,
        )
        .unwrap();

        let back = load_bound_from_file(&file, binding);
        assert!(!back.contains_key("deleted-in-scope.txt"));
        assert!(back.contains_key("excluded/keep.txt"));
        assert!(back.contains_key(".syncdash/state"));
        cleanup(&file);
    }

    #[test]
    fn an_observed_unhashed_path_invalidates_its_old_row() {
        let file = temp_file("unhashed");
        let binding = b"unhashed-root";
        write_complete(&file, binding, &[entry("changed.txt", Some("old"))]);
        let prior = load_bound_from_file(&file, binding);
        reconcile_bound_to_file(
            &file,
            binding,
            Some(prior),
            &[entry("changed.txt", None)],
            crate::store::ScanCoverage::Partial,
            &std::collections::HashSet::new(),
        )
        .unwrap();
        assert!(load_bound_from_file(&file, binding).is_empty());
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

        write_complete(&file, first.binding(), &[entry("a.txt", Some("aaa"))]);
        assert!(load_bound_from_file(&file, second.binding()).is_empty());
        assert_eq!(
            load_bound_from_file(&file, first.binding())
                .get("a.txt")
                .map(|row| row.2.as_str()),
            Some("aaa")
        );
        cleanup(&file);
    }

    #[test]
    fn nondurable_local_identity_neither_loads_nor_rewrites_persistent_cache() {
        let key = format!(
            "nondurable-hashcache-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        );
        let binding = b"injected-reused-device-binding".to_vec();
        let durable = crate::store::localid::LocalScanStateIdentity::injected(
            key.clone(),
            binding.clone(),
            true,
        );
        let nondurable = crate::store::localid::LocalScanStateIdentity::injected(
            key.clone(),
            binding.clone(),
            false,
        );
        let file = local_file_for_key(&key);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        write_complete(
            &file,
            durable.binding(),
            &[entry("old.txt", Some("old-hash"))],
        );

        assert!(load_local(&nondurable).is_empty());
        assert_eq!(
            save_local(
                &nondurable,
                &[entry("new.txt", Some("new-hash"))],
                crate::store::ScanCoverage::Complete,
                &std::collections::HashSet::new(),
            ),
            crate::store::StateWriteStatus::Unchanged
        );
        assert_eq!(
            load_local(&durable)
                .get("old.txt")
                .map(|row| row.2.as_str()),
            Some("old-hash")
        );
        assert!(!load_local(&durable).contains_key("new.txt"));
        let _ = std::fs::remove_file(file);
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

        assert!(load_bound_from_file(&file, identity.binding()).is_empty());

        write_complete(
            &file,
            identity.binding(),
            &[entry("fresh.txt", Some("fresh"))],
        );
        let rebuilt = load_bound_from_file(&file, identity.binding());
        assert!(!rebuilt.contains_key("stale.txt"));
        assert_eq!(
            rebuilt.get("fresh.txt").map(|row| row.2.as_str()),
            Some("fresh")
        );
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
        let got = local_file_for_key(&std::path::PathBuf::from(root).to_string_lossy());
        assert_eq!(
            got.file_name().unwrap().to_string_lossy(),
            format!("{expected_stem}.jsonl")
        );
    }

    #[test]
    fn an_absent_cache_is_empty_rather_than_an_error() {
        let file = temp_file("absent");
        std::fs::remove_file(&file).unwrap_or(());
        assert!(load_bound_from_file(&file, b"absent").is_empty());
        cleanup(&file);
    }
}
