//! Desktop-facing compare evidence: per-row measurements, paged identical items, and operation
//! reversal.
//!
//! `compare()` never calls into this module, and `evidence()` / `identical_page()` only describe a plan
//! that already exists — neither can change what a sync does.
//!
//! `reverse_op` is executable semantics: the desktop previews the same transformation, while Rust
//! reconstructs the authenticated operation before apply.

use serde::{Deserialize, Serialize};

use crate::model::plan::{Action, Op, Plan, Side};
use crate::model::table::{ObservedEntry, ObservedEntryKind, TableArtifact};

use std::collections::BTreeMap;

use super::keys::{files_equal, map_of};
use super::CompareOptions;
use crate::foundation::text::norm_key;

/// One side's measured state at compare time. **For display and sorting only** — apply never reads a single byte of it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub struct SideMeta {
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number")]
    pub mtime_ms: i64,
}

/// Measured state of both sides, one-to-one with `plan.ops[i]` (the absent side is None)
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub struct RowMeta {
    pub src: Option<SideMeta>,
    pub dst: Option<SideMeta>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Evidence {
    /// Length is always exactly plan.ops.len()
    pub metas: Vec<RowMeta>,
    /// Files present on both sides and judged identical by this comparison.
    pub identical_count: u64,
    pub identical_bytes: u64,
}

/// Display evidence derived from the same snapshots and exact comparison options as the plan.
///
/// Per-side measurements remain parallel to `Plan.ops` because they are presentation metadata, not
/// executable operation fields; putting them on `Op` would also change the plan JSONL format and CLI
/// contract. Identical totals use the comparison's own key folding and equality predicate.
pub fn evidence(
    source: &TableArtifact,
    target: &TableArtifact,
    plan: &Plan,
    copts: &CompareOptions,
) -> Evidence {
    evidence_for_operations(source, target, &plan.ops, copts)
}

pub fn evidence_for_operations(
    source: &TableArtifact,
    target: &TableArtifact,
    operations: &[Op],
    copts: &CompareOptions,
) -> Evidence {
    let ci = copts.case_insensitive;
    let win = copts.mtime_window_ms;
    let (s_files, _) = map_of(source, ObservedEntryKind::File, ci);
    let (t_files, _) = map_of(target, ObservedEntryKind::File, ci);
    let (s_dirs, _) = map_of(source, ObservedEntryKind::Directory, ci);
    let (t_dirs, _) = map_of(target, ObservedEntryKind::Directory, ci);

    let meta = |entry: &ObservedEntry| SideMeta {
        size: entry.size(),
        mtime_ms: entry.mtime_ms(),
    };
    let look = |files: &BTreeMap<String, &ObservedEntry>,
                dirs: &BTreeMap<String, &ObservedEntry>,
                rel: &str|
     -> Option<SideMeta> {
        let k = norm_key(rel, ci);
        files.get(&k).or_else(|| dirs.get(&k)).map(|e| meta(e))
    };

    let metas = operations
        .iter()
        .map(|op| {
            // On the executing side a move is still called from, on the other side it is already path — each side is looked up under its own name
            let (s_rel, t_rel) = match (&op.action, &op.side) {
                (Action::Move, Side::Target) => {
                    (op.path.as_str(), op.from.as_deref().unwrap_or(&op.path))
                }
                (Action::Move, Side::Source) => {
                    (op.from.as_deref().unwrap_or(&op.path), op.path.as_str())
                }
                _ => (op.path.as_str(), op.path.as_str()),
            };
            RowMeta {
                src: look(&s_files, &s_dirs, s_rel),
                dst: look(&t_files, &t_dirs, t_rel),
            }
        })
        .collect();

    let mut identical_count = 0u64;
    let mut identical_bytes = 0u64;
    for (k, se) in &s_files {
        if let Some(te) = t_files.get(k) {
            if files_equal(se, te, win) {
                let source_file = se
                    .as_file()
                    .expect("the file observation map contains only files");
                identical_count += 1;
                identical_bytes += source_file.size;
            }
        }
    }
    Evidence {
        metas,
        identical_count,
        identical_bytes,
    }
}

/// One "identical on both sides" record. It is not in the plan — it is not an action, it is evidence.
#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub struct IdenticalRow {
    pub path: String,
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number")]
    pub source_mtime_ms: i64,
    #[ts(type = "number")]
    pub target_mtime_ms: i64,
}

/// Files judged identical on both sides, paged in source-side path order. This retained evidence lets
/// the UI distinguish an identical file from a path absent because it was excluded or unread.
pub fn identical_page(
    source: &TableArtifact,
    target: &TableArtifact,
    compare_options: &CompareOptions,
    query: &str,
    offset: usize,
    limit: usize,
) -> (u64, Vec<IdenticalRow>) {
    let case_insensitive = compare_options.case_insensitive;
    let mtime_window_ms = compare_options.mtime_window_ms;
    let (source_files, _) = map_of(source, ObservedEntryKind::File, case_insensitive);
    let (target_files, _) = map_of(target, ObservedEntryKind::File, case_insensitive);
    let normalized_query = query.trim().to_lowercase();
    let mut total = 0u64;
    let mut rows = Vec::new();
    for (normalized_path, source_entry) in &source_files {
        let Some(target_entry) = target_files.get(normalized_path) else {
            continue;
        };
        if !files_equal(source_entry, target_entry, mtime_window_ms) {
            continue;
        }
        let source_file = source_entry
            .as_file()
            .expect("the source file observation map contains only files");
        let target_file = target_entry
            .as_file()
            .expect("the target file observation map contains only files");
        if !normalized_query.is_empty()
            && !source_file
                .path
                .as_str()
                .to_lowercase()
                .contains(&normalized_query)
        {
            continue;
        }
        total += 1;
        let result_index = (total - 1) as usize;
        if result_index >= offset && rows.len() < limit {
            rows.push(IdenticalRow {
                path: source_file.path.as_str().to_owned(),
                size: source_file.size,
                source_mtime_ms: source_file.mtime_ms,
                target_mtime_ms: target_file.mtime_ms,
            });
        }
    }
    (total, rows)
}

/// Reconstructs a user-reversed operation. Move, directory, conflict, and note operations are not
/// reversible.
///
/// Each arm overrides a clone so new `Op` fields survive by default. Rust invokes this function
/// while reconstructing authenticated row decisions; the TypeScript preview mirrors its semantics.
pub fn reverse_op(operation: &Op) -> Option<Op> {
    let opposite_side = match operation.side {
        Side::Source => Side::Target,
        Side::Target => Side::Source,
    };
    let reason = format!("reversed({})", operation.reason);
    match operation.action {
        // Copy becomes Delete: the content evidence describes a file that is about to be removed,
        // not written, so hash and mtime are dropped on purpose. `size` stays — it is what the
        // deletion tally is measured in.
        Action::Copy => Some(Op {
            side: opposite_side,
            action: Action::Delete,
            from: None,
            mtime_ms: None,
            hash: None,
            link: None,
            reason,
            ..operation.clone()
        }),
        // The other side's content wins, so this op can no longer describe *this* side's bytes.
        Action::Update => Some(Op {
            side: opposite_side,
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            reason,
            ..operation.clone()
        }),
        Action::Delete => Some(Op {
            side: opposite_side,
            action: Action::Copy,
            from: None,
            mtime_ms: None,
            hash: None,
            reason,
            ..operation.clone()
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{compare, CompareOptions};
    use super::*;
    use crate::foundation::path::RootRelativePath;
    use crate::model::table::{
        Blake3Digest, FileIdentityObservation, ObservedFile, TableEvidence, TableHeader, TableKind,
        TABLE_SCHEMA,
    };

    /// A reversed row is executed, not just displayed, so the reversal must not quietly drop fields.
    /// `link` is the sharp one: a symlink op that loses it stops being a symlink op and takes the
    /// content-copy lane instead. `mode` is inert only until Copy/Update start carrying it, at which
    /// point the loss would be *selective* — the one row the user looked hardest at is the one
    /// written without its mode.
    #[test]
    fn a_reversed_op_keeps_the_fields_that_decide_how_it_is_written() {
        let source_operation = Op {
            side: Side::Target,
            action: Action::Update,
            path: "bin/node".into(),
            from: None,
            size: Some(10),
            mtime_ms: Some(5),
            hash: Some("abc".into()),
            link: Some("../nodejs/bin/node".into()),
            mode: Some(0o755),
            reason: "differs".into(),
        };
        let reversed = reverse_op(&source_operation).expect("an Update is reversible");
        assert_eq!(
            reversed.side,
            Side::Source,
            "the reversal is what the side is for"
        );
        assert_eq!(
            reversed.link.as_deref(),
            Some("../nodejs/bin/node"),
            "a symlink op must stay a symlink op"
        );
        assert_eq!(reversed.mode, Some(0o755), "the mode survives the reversal");
        assert_eq!(reversed.path, "bin/node");
        // The content evidence belonged to the side that just lost, so it is dropped deliberately.
        assert_eq!(reversed.hash, None);
        assert_eq!(reversed.size, None);
    }

    fn snap(os: &str, entries: Vec<ObservedEntry>) -> TableArtifact {
        TableArtifact {
            header: TableHeader {
                schema: TABLE_SCHEMA,
                kind: TableKind::Snapshot,
                root: "/r".into(),
                host: "h".into(),
                os: os.into(),
                scanned_at_ms: 0,
                duration_ms: 0,
                entry_count: entries.len() as u64,
                evidence: TableEvidence::Full,
                excluded_dirs: 0,
                excluded_files: 0,
                walk_errors: 0,
                walk_err_samples: Vec::new(),
                icloud_stubs: 0,
                icloud_stub_samples: Vec::new(),
                dataless_files: 0,
                skipped_symlinks: 0,
                vfs: None,
            },
            entries,
        }
    }
    fn file(path: &str, hash: &str) -> ObservedEntry {
        file_with_metadata(path, hash, 1, 0)
    }

    fn file_with_metadata(path: &str, hash: &str, size: u64, mtime_ms: i64) -> ObservedEntry {
        ObservedEntry::File(ObservedFile {
            path: RootRelativePath::try_from(path).unwrap(),
            size,
            mtime_ms,
            identity: FileIdentityObservation::FullBlake3 {
                digest: Blake3Digest::hash_bytes(hash.as_bytes()),
            },
            file_system_id: None,
            mode: None,
            previous_identities: Vec::new(),
        })
    }
    #[test]
    fn reverse_op_semantics() {
        let copy = Op {
            side: Side::Target,
            action: Action::Copy,
            path: "x".into(),
            from: None,
            size: Some(5),
            mtime_ms: Some(1),
            hash: None,
            link: None,
            mode: None,
            reason: "only-in-source".into(),
        };
        let r = reverse_op(&copy).unwrap();
        assert_eq!((r.side, r.action), (Side::Source, Action::Delete));

        let del = Op {
            side: Side::Target,
            action: Action::Delete,
            path: "x".into(),
            from: None,
            size: Some(5),
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "gone-from-source".into(),
        };
        let r = reverse_op(&del).unwrap();
        assert_eq!((r.side, r.action), (Side::Source, Action::Copy));

        let upd = Op {
            side: Side::Target,
            action: Action::Update,
            path: "x".into(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "differs".into(),
        };
        let r = reverse_op(&upd).unwrap();
        assert_eq!((r.side, r.action), (Side::Source, Action::Update));

        let mv = Op {
            side: Side::Target,
            action: Action::Move,
            path: "b".into(),
            from: Some("a".into()),
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "m".into(),
        };
        assert!(reverse_op(&mv).is_none());
    }

    #[test]
    fn normalization_twins_reported_not_merged() {
        let s = snap(
            "linux",
            vec![
                file("caf\u{00e9}.txt", "h1"),
                file("cafe\u{0301}.txt", "h2"),
            ],
        );
        let t = snap("windows", vec![file("caf\u{00e9}.txt", "h1")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert!(plan.ops.iter().any(
            |o| o.action == Action::Note && o.reason.contains("duplicate-after-normalization")
        ));
    }

    // Evidence layer

    #[test]
    fn evidence_reports_both_sides_and_identical_count() {
        // same: identical on both sides; upd: on both sides but with different content; only_s: source only; only_t: target only
        let s = snap(
            "windows",
            vec![
                file_with_metadata("same.txt", "h0", 10, 1_000),
                file_with_metadata("upd.txt", "hs", 30, 9_000),
                file_with_metadata("only_s.txt", "h1", 7, 5_000),
            ],
        );
        let t = snap(
            "windows",
            vec![
                file_with_metadata("same.txt", "h0", 10, 1_000),
                file_with_metadata("upd.txt", "ht", 20, 2_000),
                file_with_metadata("only_t.txt", "h2", 4, 3_000),
            ],
        );
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let ev = evidence(&s, &t, &plan, &CompareOptions::default());
        assert_eq!(
            ev.metas.len(),
            plan.ops.len(),
            "the evidence array must correspond one-to-one with ops"
        );
        assert_eq!(ev.identical_count, 1);
        assert_eq!(ev.identical_bytes, 10);

        let by = |name: &str| {
            let i = plan.ops.iter().position(|o| o.path == name).expect(name);
            ev.metas[i].clone()
        };
        // update row: both sides need their own measured values — today Op.size/mtime can only carry source's
        let m = by("upd.txt");
        assert_eq!(m.src.unwrap().size, 30);
        assert_eq!(m.src.unwrap().mtime_ms, 9_000);
        assert_eq!(m.dst.unwrap().size, 20);
        assert_eq!(m.dst.unwrap().mtime_ms, 2_000);
        // copy row: only the source side exists
        let m = by("only_s.txt");
        assert_eq!(m.src.unwrap().size, 7);
        assert!(m.dst.is_none());
        // delete row: only the target side exists
        let m = by("only_t.txt");
        assert!(m.src.is_none());
        assert_eq!(m.dst.unwrap().size, 4);
    }

    #[test]
    fn identical_page_lists_only_identical_files_and_pages() {
        let make_entry = |index: usize, hash: &str| {
            file_with_metadata(
                &format!("d{}/f{index}.bin", index % 3),
                hash,
                index as u64,
                index as i64 * 1000,
            )
        };
        let source = snap(
            "windows",
            (0..10).map(|index| make_entry(index, "same")).collect(),
        );
        // The last 3 differ in content on the target side → not counted as equal
        let target = snap(
            "windows",
            (0..10)
                .map(|index| make_entry(index, if index >= 7 { "diff" } else { "same" }))
                .collect(),
        );
        let compare_options = CompareOptions::default();
        let (total, rows) = identical_page(&source, &target, &compare_options, "", 0, 100);
        assert_eq!(total, 7);
        let expected_paths = vec![
            "d0/f0.bin",
            "d0/f3.bin",
            "d0/f6.bin",
            "d1/f1.bin",
            "d1/f4.bin",
            "d2/f2.bin",
            "d2/f5.bin",
        ];
        assert_eq!(
            rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
            expected_paths
        );
        let (total, second_page) = identical_page(&source, &target, &compare_options, "", 3, 3);
        assert_eq!(
            total, 7,
            "total is the post-filter total, independent of the paging window"
        );
        assert_eq!(
            second_page
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            &expected_paths[3..6]
        );
        let (_, first_page) = identical_page(&source, &target, &compare_options, "", 0, 3);
        assert_eq!(
            first_page
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            &expected_paths[..3]
        );
        assert!(first_page
            .iter()
            .all(|first| second_page.iter().all(|second| first.path != second.path)));
        let (total, rows) = identical_page(&source, &target, &compare_options, "D1/", 0, 100);
        assert_eq!(total as usize, rows.len());
        assert!(rows.iter().all(|row| row.path.starts_with("d1/")));
        assert!(rows
            .iter()
            .all(|row| row.source_mtime_ms == row.target_mtime_ms));
    }

    #[test]
    fn evidence_follows_move_naming_on_each_side() {
        // Same-content rename: source is already called b.bin, target is still a.bin — each side is looked up under its own name
        let s = snap(
            "windows",
            vec![file_with_metadata("b.bin", "hm", 42, 8_000)],
        );
        let t = snap(
            "windows",
            vec![file_with_metadata("a.bin", "hm", 42, 4_000)],
        );
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let i = plan
            .ops
            .iter()
            .position(|o| o.action == Action::Move)
            .expect("move");
        let ev = evidence(&s, &t, &plan, &CompareOptions::default());
        assert_eq!(
            ev.metas[i].src.unwrap().mtime_ms,
            8_000,
            "the source side is looked up under the new name b.bin"
        );
        assert_eq!(
            ev.metas[i].dst.unwrap().mtime_ms,
            4_000,
            "the target side is looked up under the old name a.bin"
        );
    }
}
