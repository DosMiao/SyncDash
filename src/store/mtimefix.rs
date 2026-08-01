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
use std::path::{Path, PathBuf};

const STATE_KIND: &str = "mtimefix";

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
fn load_bound_from_file(
    file: &Path,
    binding: &[u8],
    legacy: super::LegacyPolicy,
) -> HashMap<String, (i64, i64)> {
    let mut map = HashMap::new();
    let accepted = super::read_scan_state(
        file,
        STATE_KIND,
        binding,
        legacy,
        |c: MtimeFix| {
            map.insert(c.path, (c.ondisk_ms, c.intended_ms));
        },
    );
    if !accepted {
        map.clear();
    }
    map
}

fn load_from_file(file: &Path, key: &str) -> HashMap<String, (i64, i64)> {
    load_bound_from_file(
        file,
        &super::logical_scan_state_binding(key),
        super::LegacyPolicy::Accept,
    )
}

pub fn load_by_key(key: &str) -> HashMap<String, (i64, i64)> {
    load_from_file(&file_for_key(key), key)
}

pub fn load(root: &Path) -> HashMap<String, (i64, i64)> {
    let identity = super::localid::LocalScanStateIdentity::for_root(root);
    load_local(&identity)
}

pub(crate) fn load_local(
    identity: &super::localid::LocalScanStateIdentity,
) -> HashMap<String, (i64, i64)> {
    load_bound_from_file(
        &file_for_key(identity.cache_key()),
        identity.binding(),
        super::LegacyPolicy::Reject,
    )
}

fn rewrite_map(
    file: &Path,
    binding: &[u8],
    map: &HashMap<String, (i64, i64)>,
    keep: impl Fn(&str) -> bool,
) -> std::io::Result<bool> {
    super::rewrite_scan_state(file, STATE_KIND, binding, |writer| {
        for (path, (ondisk_ms, intended_ms)) in map {
            if keep(path) {
                let rec = MtimeFix {
                    path: path.clone(),
                    ondisk_ms: *ondisk_ms,
                    intended_ms: *intended_ms,
                };
                serde_json::to_writer(&mut *writer, &rec).map_err(std::io::Error::other)?;
                writer.write_all(b"\n")?;
            }
        }
        Ok(())
    })
}

/// Merge corrections in and rewrite wholesale, rather than appending forever.
fn record_bound_file(
    file: &Path,
    binding: &[u8],
    legacy: super::LegacyPolicy,
    fixes: &[(String, i64, i64)],
) -> std::io::Result<bool> {
    if fixes.is_empty() {
        return Ok(false);
    }
    let mut map = load_bound_from_file(file, binding, legacy);
    for (p, ondisk, intended) in fixes {
        map.insert(p.clone(), (*ondisk, *intended));
    }
    rewrite_map(file, binding, &map, |_| true)
}

fn record_file(file: &Path, key: &str, fixes: &[(String, i64, i64)]) -> std::io::Result<bool> {
    record_bound_file(
        file,
        &super::logical_scan_state_binding(key),
        super::LegacyPolicy::Accept,
        fixes,
    )
}

pub fn record_by_key(key: &str, fixes: &[(String, i64, i64)]) {
    let _ = record_file(&file_for_key(key), key, fixes);
}

pub(crate) fn record_local(
    identity: &super::localid::LocalScanStateIdentity,
    fixes: &[(String, i64, i64)],
) {
    let _ = record_bound_file(
        &file_for_key(identity.cache_key()),
        identity.binding(),
        super::LegacyPolicy::Reject,
        fixes,
    );
}

fn prune_bound_file(
    file: &Path,
    binding: &[u8],
    fixes: &HashMap<String, (i64, i64)>,
    entries: &[crate::model::table::Entry],
) -> std::io::Result<bool> {
    if fixes.is_empty() {
        return Ok(false);
    }
    let live: std::collections::HashSet<&str> =
        entries.iter().map(|entry| entry.path.as_str()).collect();
    if fixes.keys().all(|path| live.contains(path.as_str())) {
        return Ok(false);
    }
    rewrite_map(file, binding, fixes, |path| live.contains(path))
}

fn prune_file(
    file: &Path,
    key: &str,
    fixes: &HashMap<String, (i64, i64)>,
    entries: &[crate::model::table::Entry],
) -> std::io::Result<bool> {
    prune_bound_file(
        file,
        &super::logical_scan_state_binding(key),
        fixes,
        entries,
    )
}

pub fn prune_by_key(
    key: &str,
    fixes: &HashMap<String, (i64, i64)>,
    entries: &[crate::model::table::Entry],
) {
    let _ = prune_file(&file_for_key(key), key, fixes, entries);
}

pub(crate) fn prune_local(
    identity: &super::localid::LocalScanStateIdentity,
    fixes: &HashMap<String, (i64, i64)>,
    entries: &[crate::model::table::Entry],
) {
    let file = file_for_key(identity.cache_key());
    let _ = prune_local_file(&file, identity.binding(), fixes, entries);
}

fn prune_local_file(
    file: &Path,
    binding: &[u8],
    fixes: &HashMap<String, (i64, i64)>,
    entries: &[crate::model::table::Entry],
) -> std::io::Result<bool> {
    if fixes.is_empty() && super::scan_state_needs_rebuild(file, STATE_KIND, binding) {
        // A completed local scan establishes the physical volume provenance even when it has no
        // corrections to retain. Replace a legacy or mismatched generation with a header-only one
        // so it cannot be reconsidered on every subsequent scan.
        return rewrite_map(file, binding, fixes, |_| true);
    }
    prune_bound_file(file, binding, fixes, entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("syncdash-mtimefix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory.join("state.jsonl")
    }

    fn cleanup(file: &Path) {
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_correction_survives_and_later_writes_merge_rather_than_append() {
        let file = temp_file("merge");
        let key = "mtimefix-test-root";
        record_file(&file, key, &[("a.txt".into(), 1_000, 1_500)]).unwrap();
        record_file(&file, key, &[("b.txt".into(), 2_000, 2_500)]).unwrap();
        // Re-correcting a path replaces its row instead of leaving two to disagree
        record_file(&file, key, &[("a.txt".into(), 9_000, 9_500)]).unwrap();

        let back = load_from_file(&file, key);
        assert_eq!(back.len(), 2, "one row per path, not one per write");
        assert_eq!(
            back.get("a.txt"),
            Some(&(9_000, 9_500)),
            "the newest correction wins"
        );
        assert_eq!(back.get("b.txt"), Some(&(2_000, 2_500)));
        cleanup(&file);
    }

    #[test]
    fn recording_nothing_does_not_create_a_file() {
        let file = temp_file("empty");
        assert!(!record_file(&file, "mtimefix-empty-root", &[]).unwrap());
        assert!(!file.exists());
        cleanup(&file);
    }

    #[test]
    fn pruning_drops_only_paths_absent_from_a_complete_snapshot() {
        let file = temp_file("prune");
        let key = "mtimefix-prune-root";
        record_file(
            &file,
            key,
            &[("live.txt".into(), 1_000, 1_500), ("gone.txt".into(), 2_000, 2_500)],
        )
        .unwrap();
        let fixes = load_from_file(&file, key);
        let live = crate::model::table::Entry {
            path: "live.txt".into(),
            kind: crate::model::table::EntryKind::File,
            size: 1,
            mtime_ms: 1_500,
            hash: None,
            hash_failed: false,
            file_id: None,
            mode: None,
            link: None,
            prev: None,
        };

        prune_file(&file, key, &fixes, &[live]).unwrap();

        let back = load_from_file(&file, key);
        assert_eq!(back.len(), 1);
        assert_eq!(back.get("live.txt"), Some(&(1_000, 1_500)));
        cleanup(&file);
    }

    #[test]
    fn legacy_corrections_are_accepted_and_migrate_during_the_next_merge() {
        let file = temp_file("legacy");
        let key = "mtimefix-legacy-root";
        std::fs::write(
            &file,
            "{\"path\":\"old.txt\",\"ondisk_ms\":1000,\"intended_ms\":1500}\n",
        )
        .unwrap();
        assert_eq!(
            load_from_file(&file, key).get("old.txt"),
            Some(&(1_000, 1_500))
        );

        record_file(&file, key, &[("new.txt".into(), 2_000, 2_500)]).unwrap();
        let migrated = load_from_file(&file, key);
        assert_eq!(migrated.len(), 2);
        let text = std::fs::read_to_string(&file).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["schema"], "syncdash.scan-state");
        assert_eq!(first["version"], 1);
        assert_eq!(first["kind"], STATE_KIND);
        cleanup(&file);
    }

    #[test]
    fn corrections_bound_to_another_root_are_not_applied() {
        let file = temp_file("binding");
        record_file(&file, "root-a", &[("a.txt".into(), 1_000, 1_500)]).unwrap();
        assert!(load_from_file(&file, "root-b").is_empty());
        cleanup(&file);
    }

    #[test]
    fn local_headerless_corrections_are_rejected_then_replaced_after_a_successful_scan() {
        let file = temp_file("local-legacy");
        let root = file.parent().unwrap().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let identity = crate::store::localid::LocalScanStateIdentity::for_root(&root);
        std::fs::write(
            &file,
            "{\"path\":\"stale.txt\",\"ondisk_ms\":1000,\"intended_ms\":1500}\n",
        )
        .unwrap();

        let fixes = load_bound_from_file(
            &file,
            identity.binding(),
            crate::store::LegacyPolicy::Reject,
        );
        assert!(fixes.is_empty());
        assert!(prune_local_file(&file, identity.binding(), &fixes, &[]).unwrap());
        assert!(load_bound_from_file(
            &file,
            identity.binding(),
            crate::store::LegacyPolicy::Reject,
        )
        .is_empty());
        assert!(!crate::store::scan_state_needs_rebuild(
            &file,
            STATE_KIND,
            identity.binding(),
        ));
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.lines().count(), 1, "the stale legacy row was discarded");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap()).unwrap()
                ["schema"],
            "syncdash.scan-state"
        );
        cleanup(&file);
    }
}
