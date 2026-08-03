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
//! Reconciliation follows scan coverage and correction provenance. A partial table retains unseen
//! corrections, but an observed object whose raw timestamp no longer matches its correction always
//! invalidates that row. An error-free scan also removes absent in-domain corrections.

use std::collections::HashMap;
use std::path::Path;

use super::scan_state::bound::BoundTable;

const STATE_KIND: &str = "mtimefix";

/// What the filesystem reports for an object, and what the write that produced it intended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MtimeCorrection {
    pub ondisk_ms: i64,
    pub intended_ms: i64,
}

pub type MtimeCorrections = HashMap<String, MtimeCorrection>;

#[derive(serde::Serialize, serde::Deserialize)]
struct MtimeFix {
    path: String,
    ondisk_ms: i64,
    intended_ms: i64,
}

fn keep_correction(map: &mut MtimeCorrections, row: MtimeFix) {
    map.insert(
        row.path,
        MtimeCorrection {
            ondisk_ms: row.ondisk_ms,
            intended_ms: row.intended_ms,
        },
    );
}

const TABLE: BoundTable<String, MtimeCorrection, MtimeFix> =
    BoundTable::new(STATE_KIND, "mtimefix.jsonl", keep_correction);

pub fn load_by_key(key: &str) -> MtimeCorrections {
    TABLE.load_by_key(key)
}

pub(crate) fn load_local(identity: &super::localid::LocalScanStateIdentity) -> MtimeCorrections {
    TABLE.load_local(identity)
}

fn rewrite_map(
    file: &Path,
    binding: &[u8],
    map: &MtimeCorrections,
    keep: impl Fn(&str) -> bool,
) -> std::io::Result<bool> {
    TABLE.rewrite(file, binding, map, |writer, path, correction| {
        if !keep(path) {
            return Ok(());
        }
        let rec = MtimeFix {
            path: path.clone(),
            ondisk_ms: correction.ondisk_ms,
            intended_ms: correction.intended_ms,
        };
        serde_json::to_writer(&mut *writer, &rec).map_err(std::io::Error::other)?;
        writer.write_all(b"\n")
    })
}

/// Merge corrections in and rewrite wholesale, rather than appending forever.
fn record_bound_file(
    file: &Path,
    binding: &[u8],
    mut map: MtimeCorrections,
    fixes: &[(String, i64, i64)],
) -> std::io::Result<bool> {
    for (p, ondisk, intended) in fixes {
        map.insert(
            p.clone(),
            MtimeCorrection {
                ondisk_ms: *ondisk,
                intended_ms: *intended,
            },
        );
    }
    rewrite_map(file, binding, &map, |_| true)
}

pub fn record_by_key(key: &str, fixes: &[(String, i64, i64)]) -> super::StateWriteStatus {
    if fixes.is_empty() {
        return super::StateWriteStatus::Unchanged;
    }
    let file = TABLE.logical_file(key);
    let Some(map) = TABLE.load_logical(key) else {
        return super::StateWriteStatus::Failed;
    };
    let result = record_bound_file(
        &file,
        &super::scan_state::binding::logical_binding(key),
        map,
        fixes,
    );
    super::scan_state::reporting::report_write(&file, result)
}

pub(crate) fn record_local(
    identity: &super::localid::LocalScanStateIdentity,
    fixes: &[(String, i64, i64)],
) -> super::StateWriteStatus {
    if fixes.is_empty() {
        return super::StateWriteStatus::Unchanged;
    }
    if !identity.persistent_reuse() {
        return super::StateWriteStatus::Unchanged;
    }
    let file = TABLE.local_file(identity.cache_key());
    let Some((map, _)) = TABLE.read_best_effort(&file, identity.binding()) else {
        return super::StateWriteStatus::Failed;
    };
    let result = record_bound_file(&file, identity.binding(), map, fixes);
    super::scan_state::reporting::report_write(&file, result)
}

fn reconcile_bound_file(
    file: &Path,
    binding: &[u8],
    fixes: &MtimeCorrections,
    entries: &[crate::model::table::ObservedEntry],
    coverage: super::ScanCoverage,
    matched: &std::collections::HashSet<String>,
    retain_absent: &std::collections::HashSet<String>,
) -> std::io::Result<Option<bool>> {
    let needs_rebuild = super::scan_state::needs_rebuild(file, STATE_KIND, binding);
    let needs_materialize = !file.exists() && !fixes.is_empty();
    let observed: std::collections::HashSet<&str> =
        entries.iter().map(|entry| entry.path().as_str()).collect();
    let live_files: std::collections::HashSet<&str> = entries
        .iter()
        .filter(|entry| entry.as_file().is_some())
        .map(|entry| entry.path().as_str())
        .collect();
    let keep = |path: &str| {
        if observed.contains(path) {
            live_files.contains(path) && matched.contains(path)
        } else if coverage == super::ScanCoverage::Partial {
            true
        } else {
            retain_absent.contains(path)
        }
    };
    let drops_rows = fixes.keys().any(|path| !keep(path));
    if !drops_rows && !needs_rebuild && !needs_materialize {
        return Ok(None);
    }
    rewrite_map(file, binding, fixes, keep).map(Some)
}

pub fn reconcile_by_key(
    key: &str,
    fixes: &MtimeCorrections,
    entries: &[crate::model::table::ObservedEntry],
    coverage: super::ScanCoverage,
    matched: &std::collections::HashSet<String>,
    retain_absent: &std::collections::HashSet<String>,
) -> super::StateWriteStatus {
    let file = TABLE.logical_file(key);
    match reconcile_bound_file(
        &file,
        &super::scan_state::binding::logical_binding(key),
        fixes,
        entries,
        coverage,
        matched,
        retain_absent,
    ) {
        Ok(None) => super::StateWriteStatus::Unchanged,
        Ok(Some(result)) => super::scan_state::reporting::report_write(&file, Ok(result)),
        Err(error) => super::scan_state::reporting::report_write(&file, Err(error)),
    }
}

pub(crate) fn reconcile_local(
    identity: &super::localid::LocalScanStateIdentity,
    fixes: &MtimeCorrections,
    entries: &[crate::model::table::ObservedEntry],
    coverage: super::ScanCoverage,
    matched: &std::collections::HashSet<String>,
    retain_absent: &std::collections::HashSet<String>,
) -> super::StateWriteStatus {
    if !identity.persistent_reuse() {
        return super::StateWriteStatus::Unchanged;
    }
    let file = TABLE.local_file(identity.cache_key());
    match reconcile_bound_file(
        &file,
        identity.binding(),
        fixes,
        entries,
        coverage,
        matched,
        retain_absent,
    ) {
        Ok(None) => super::StateWriteStatus::Unchanged,
        Ok(Some(result)) => super::scan_state::reporting::report_write(&file, Ok(result)),
        Err(error) => super::scan_state::reporting::report_write(&file, Err(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    fn entry(path: &str) -> crate::model::table::ObservedEntry {
        crate::model::table::ObservedEntry::File(crate::model::table::ObservedFile {
            path: crate::foundation::path::RootRelativePath::try_from(path).unwrap(),
            size: 1,
            mtime_ms: 1_500,
            identity: crate::model::table::FileIdentityObservation::SizeAndMtime,
            file_system_id: None,
            mode: None,
            previous_identities: Vec::new(),
        })
    }

    fn record_file(file: &Path, key: &str, fixes: &[(String, i64, i64)]) {
        let binding = crate::store::scan_state::binding::logical_binding(key);
        let map = TABLE.load_file(file, &binding);
        record_bound_file(file, &binding, map, fixes).unwrap();
    }

    #[test]
    fn a_correction_survives_and_later_writes_merge_rather_than_append() {
        let file = temp_file("merge");
        let key = "mtimefix-test-root";
        record_file(&file, key, &[("a.txt".into(), 1_000, 1_500)]);
        record_file(&file, key, &[("b.txt".into(), 2_000, 2_500)]);
        record_file(&file, key, &[("a.txt".into(), 9_000, 9_500)]);

        let back = TABLE.load_file(
            &file,
            &crate::store::scan_state::binding::logical_binding(key),
        );
        assert_eq!(back.len(), 2, "one row per path, not one per write");
        assert_eq!(
            back.get("a.txt"),
            Some(&MtimeCorrection {
                ondisk_ms: 9_000,
                intended_ms: 9_500,
            }),
            "the newest correction wins"
        );
        assert_eq!(
            back.get("b.txt"),
            Some(&MtimeCorrection {
                ondisk_ms: 2_000,
                intended_ms: 2_500,
            })
        );
        cleanup(&file);
    }

    #[test]
    fn partial_reconciliation_retains_excluded_rows_and_complete_drops_deleted_rows() {
        let file = temp_file("coverage");
        let key = "mtimefix-prune-root";
        record_file(
            &file,
            key,
            &[
                ("live.txt".into(), 1_000, 1_500),
                ("gone.txt".into(), 2_000, 2_500),
            ],
        );
        let binding = crate::store::scan_state::binding::logical_binding(key);
        let fixes = TABLE.load_file(&file, &binding);
        let matched = std::collections::HashSet::from(["live.txt".to_string()]);

        assert_eq!(
            reconcile_bound_file(
                &file,
                &binding,
                &fixes,
                &[entry("live.txt")],
                crate::store::ScanCoverage::Partial,
                &matched,
                &std::collections::HashSet::new(),
            )
            .unwrap(),
            None
        );
        assert_eq!(TABLE.load_file(&file, &binding).len(), 2);

        assert!(reconcile_bound_file(
            &file,
            &binding,
            &fixes,
            &[entry("live.txt")],
            crate::store::ScanCoverage::Complete,
            &matched,
            &std::collections::HashSet::new(),
        )
        .unwrap()
        .unwrap());
        let back = TABLE.load_file(&file, &binding);
        assert_eq!(back.len(), 1);
        assert_eq!(
            back.get("live.txt"),
            Some(&MtimeCorrection {
                ondisk_ms: 1_000,
                intended_ms: 1_500,
            })
        );
        cleanup(&file);
    }

    #[test]
    fn an_observed_mismatch_invalidates_a_correction_even_after_a_partial_walk() {
        let file = temp_file("mismatch");
        let key = "mtimefix-mismatch-root";
        record_file(
            &file,
            key,
            &[
                ("changed.txt".into(), 1_000, 1_500),
                ("unseen.txt".into(), 2_000, 2_500),
            ],
        );
        let binding = crate::store::scan_state::binding::logical_binding(key);
        let fixes = TABLE.load_file(&file, &binding);

        reconcile_bound_file(
            &file,
            &binding,
            &fixes,
            &[entry("changed.txt")],
            crate::store::ScanCoverage::Partial,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .unwrap();

        let back = TABLE.load_file(&file, &binding);
        assert!(!back.contains_key("changed.txt"));
        assert!(back.contains_key("unseen.txt"));
        cleanup(&file);
    }

    #[test]
    fn an_error_free_scan_keeps_an_absent_correction_only_outside_its_domain() {
        let file = temp_file("domain");
        let key = "mtimefix-domain-root";
        record_file(
            &file,
            key,
            &[
                ("deleted.txt".into(), 1_000, 1_500),
                ("excluded/keep.txt".into(), 2_000, 2_500),
            ],
        );
        let binding = crate::store::scan_state::binding::logical_binding(key);
        let fixes = TABLE.load_file(&file, &binding);
        let retain = std::collections::HashSet::from(["excluded/keep.txt".to_string()]);

        reconcile_bound_file(
            &file,
            &binding,
            &fixes,
            &[],
            crate::store::ScanCoverage::Complete,
            &std::collections::HashSet::new(),
            &retain,
        )
        .unwrap();

        let back = TABLE.load_file(&file, &binding);
        assert!(!back.contains_key("deleted.txt"));
        assert!(back.contains_key("excluded/keep.txt"));
        cleanup(&file);
    }

    #[test]
    fn headerless_corrections_are_rejected() {
        let file = temp_file("headerless");
        std::fs::write(
            &file,
            "{\"path\":\"old.txt\",\"ondisk_ms\":1000,\"intended_ms\":1500}\n",
        )
        .unwrap();
        assert!(TABLE.load_file(&file, b"headerless-root").is_empty());
        cleanup(&file);
    }

    #[test]
    fn corrections_bound_to_another_root_are_not_applied() {
        let file = temp_file("binding");
        record_file(&file, "root-a", &[("a.txt".into(), 1_000, 1_500)]);
        assert!(TABLE
            .load_file(
                &file,
                &crate::store::scan_state::binding::logical_binding("root-b")
            )
            .is_empty());
        cleanup(&file);
    }

    #[test]
    fn nondurable_local_identity_neither_loads_nor_rewrites_mtime_corrections() {
        let key = format!(
            "nondurable-mtimefix-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        );
        let binding = b"injected-reused-device-binding".to_vec();
        let durable = crate::store::localid::LocalScanStateIdentity::injected(
            key.clone(),
            binding.clone(),
            true,
        );
        let nondurable =
            crate::store::localid::LocalScanStateIdentity::injected(key.clone(), binding, false);
        let file = TABLE.local_file(&key);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        record_bound_file(
            &file,
            durable.binding(),
            HashMap::new(),
            &[("old.txt".into(), 1_000, 1_500)],
        )
        .unwrap();

        assert!(load_local(&nondurable).is_empty());
        assert_eq!(
            record_local(&nondurable, &[("new.txt".into(), 2_000, 2_500)]),
            crate::store::StateWriteStatus::Unchanged
        );
        assert_eq!(
            reconcile_local(
                &nondurable,
                &HashMap::new(),
                &[],
                crate::store::ScanCoverage::Complete,
                &std::collections::HashSet::new(),
                &std::collections::HashSet::new(),
            ),
            crate::store::StateWriteStatus::Unchanged
        );
        let loaded = load_local(&durable);
        assert_eq!(
            loaded.get("old.txt"),
            Some(&MtimeCorrection {
                ondisk_ms: 1_000,
                intended_ms: 1_500,
            })
        );
        assert!(!loaded.contains_key("new.txt"));
        let _ = std::fs::remove_file(file);
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

        let fixes = TABLE.load_file(&file, identity.binding());
        assert!(fixes.is_empty());
        assert!(reconcile_bound_file(
            &file,
            identity.binding(),
            &fixes,
            &[],
            crate::store::ScanCoverage::Partial,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )
        .unwrap()
        .unwrap());
        assert!(TABLE.load_file(&file, identity.binding()).is_empty());
        assert!(!crate::store::scan_state::needs_rebuild(
            &file,
            STATE_KIND,
            identity.binding(),
        ));
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            text.lines().count(),
            1,
            "the stale legacy row was discarded"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap()).unwrap()
                ["schema"],
            "syncdash.scan-state"
        );
        cleanup(&file);
    }
}
