//! The read-only layer the desktop reads: per-row measurements, the paged "identical items" view,
//! and the reverse of an op for the UI's direction flip.
//!
//! `compare()` never calls into this module, and `evidence()` / `same_page()` only describe a plan
//! that already exists — neither can change what a sync does.
//!
//! `reverse_op` is the exception, and the header used to claim otherwise. Its output is not a
//! description: the desktop mirrors these semantics lazily when a user flips a row and builds the
//! apply payload from that result. A field dropped by either implementation is a field dropped
//! from a write. That mistaken "this file is inert" line is why it went unexamined.

use serde::{Deserialize, Serialize};

use crate::model::plan::{Action, Op, Plan, Side};
use crate::model::table::{Entry, Snapshot};

use std::collections::BTreeMap;

use crate::foundation::text::norm_key;
use crate::model::table::EntryKind;

use super::keys::{files_equal, map_of};
use super::CompareOptions;

/// One side's measured state at compare time. **For display and sorting only** — apply never reads a single byte of it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct SideMeta {
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number")]
    pub mtime_ms: i64,
}

/// Measured state of both sides, one-to-one with `plan.ops[i]` (the absent side is None)
#[derive(Serialize, Deserialize, Clone, Default, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct RowMeta {
    pub src: Option<SideMeta>,
    pub dst: Option<SideMeta>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Evidence {
    /// Length is always exactly plan.ops.len()
    pub metas: Vec<RowMeta>,
    /// Files present on both sides and judged equal — the source of the denominator in FFS's "Showing 481 of 23,112"
    pub equal_count: u64,
    pub equal_bytes: u64,
}

/// "Evidence" beyond the plan: measured size/mtime of both sides per row + a count of equal items.
///
/// Why these two fields are not simply stuffed into `Op`: there are thirty-odd `Op { .. }` struct
/// literals in this file, so adding a field means touching thirty-odd sites, and it would change the
/// plan JSONL on-disk format and the CLI behavior. We use a parallel array, leaving `compare()` untouched.
///
/// The criteria share `norm_key` / `files_equal` with `compare()`, so they cannot drift.
pub fn evidence(
    source: &Snapshot,
    target: &Snapshot,
    plan: &Plan,
    copts: &CompareOptions,
) -> Evidence {
    let ci = copts.case_insensitive;
    let win = copts.mtime_window_ms;
    let (s_files, _) = map_of(source, EntryKind::File, ci);
    let (t_files, _) = map_of(target, EntryKind::File, ci);
    let (s_dirs, _) = map_of(source, EntryKind::Dir, ci);
    let (t_dirs, _) = map_of(target, EntryKind::Dir, ci);

    let meta = |e: &Entry| SideMeta {
        size: e.size,
        mtime_ms: e.mtime_ms,
    };
    let look = |files: &BTreeMap<String, &Entry>,
                dirs: &BTreeMap<String, &Entry>,
                rel: &str|
     -> Option<SideMeta> {
        let k = norm_key(rel, ci);
        files.get(&k).or_else(|| dirs.get(&k)).map(|e| meta(e))
    };

    let metas = plan
        .ops
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

    let mut equal_count = 0u64;
    let mut equal_bytes = 0u64;
    for (k, se) in &s_files {
        if let Some(te) = t_files.get(k) {
            if files_equal(se, te, win) {
                equal_count += 1;
                equal_bytes += se.size;
            }
        }
    }
    Evidence {
        metas,
        equal_count,
        equal_bytes,
    }
}

/// One "identical on both sides" record. It is not in the plan — it is not an action, it is evidence.
#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct SameRow {
    pub path: String,
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number")]
    pub mtime_ms: i64,
    /// The target side's time (content is identical but timestamps may differ by a few milliseconds — FAT/SMB granularity)
    #[ts(type = "number")]
    pub other_mtime_ms: i64,
}

/// Files judged equal on both sides, paged in source-side path order.
/// The data behind FFS's "22,631" button at the bottom: when a file does not appear in the diff table,
/// you must be able to confirm it is "equal" rather than "never scanned at all".
pub fn same_page(
    source: &Snapshot,
    target: &Snapshot,
    copts: &CompareOptions,
    query: &str,
    offset: usize,
    limit: usize,
) -> (u64, Vec<SameRow>) {
    let ci = copts.case_insensitive;
    let win = copts.mtime_window_ms;
    let (s_files, _) = map_of(source, EntryKind::File, ci);
    let (t_files, _) = map_of(target, EntryKind::File, ci);
    let q = query.trim().to_lowercase();
    let mut total = 0u64;
    let mut out = Vec::new();
    for (k, se) in &s_files {
        let Some(te) = t_files.get(k) else { continue };
        if !files_equal(se, te, win) {
            continue;
        }
        if !q.is_empty() && !se.path.to_lowercase().contains(&q) {
            continue;
        }
        total += 1;
        let idx = (total - 1) as usize;
        if idx >= offset && out.len() < limit {
            out.push(SameRow {
                path: se.path.clone(),
                size: se.size,
                mtime_ms: se.mtime_ms,
                other_mtime_ms: te.mtime_ms,
            });
        }
    }
    (total, out)
}

/// Per-row direction flip in the GUI (the semantic core of the same interaction FFS has). Returns None = this op cannot be reversed (move/dir/conflict/note).
/// - Reverse of Copy: instead of pushing the file over, delete the "extra" one (the side that has it falls in line with the side that lacks it)
/// - Reverse of Update: let the other side's content win
/// - Reverse of Delete: don't delete — copy it back to the other side instead (restore)
///
/// **Built by overriding a clone, not by listing fields.** Each arm used to construct an `Op` from
/// scratch, which meant every field nobody thought to name was silently dropped — `link` and `mode`
/// both arrived as `None`. That is not display-only: the frontend builds the apply payload from the
/// flipped op, so a flipped symlink Update took the content-copy lane instead of `make_symlink`.
/// Spelling it `..op.clone()` means the next field added to `Op` is carried here by default and only
/// a deliberate override can drop it.
pub fn reverse_op(op: &Op) -> Option<Op> {
    let other = match op.side {
        Side::Source => Side::Target,
        Side::Target => Side::Source,
    };
    let reason = format!("flipped({})", op.reason);
    match op.action {
        // Copy becomes Delete: the content evidence describes a file that is about to be removed,
        // not written, so hash and mtime are dropped on purpose. `size` stays — it is what the
        // deletion tally is measured in.
        Action::Copy => Some(Op {
            side: other,
            action: Action::Delete,
            from: None,
            mtime_ms: None,
            hash: None,
            link: None,
            reason,
            ..op.clone()
        }),
        // The other side's content wins, so this op can no longer describe *this* side's bytes.
        Action::Update => Some(Op {
            side: other,
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            reason,
            ..op.clone()
        }),
        Action::Delete => Some(Op {
            side: other,
            action: Action::Copy,
            from: None,
            mtime_ms: None,
            hash: None,
            reason,
            ..op.clone()
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{compare, CompareOptions};
    use super::*;
    use crate::model::table::{Header, SCHEMA};

    /// A flipped row is executed, not just displayed, so the reversal must not quietly drop fields.
    /// `link` is the sharp one: a symlink op that loses it stops being a symlink op and takes the
    /// content-copy lane instead. `mode` is inert only until Copy/Update start carrying it, at which
    /// point the loss would be *selective* — the one row the user looked hardest at is the one
    /// written without its mode.
    #[test]
    fn a_flipped_op_keeps_the_fields_that_decide_how_it_is_written() {
        let src = Op {
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
        let r = reverse_op(&src).expect("an Update is reversible");
        assert_eq!(r.side, Side::Source, "the flip is what the side is for");
        assert_eq!(
            r.link.as_deref(),
            Some("../nodejs/bin/node"),
            "a symlink op must stay a symlink op"
        );
        assert_eq!(r.mode, Some(0o755), "the mode survives the flip");
        assert_eq!(r.path, "bin/node");
        // The content evidence belonged to the side that just lost, so it is dropped deliberately.
        assert_eq!(r.hash, None);
        assert_eq!(r.size, None);
    }

    fn snap(os: &str, entries: Vec<Entry>) -> Snapshot {
        Snapshot {
            header: Header {
                schema: SCHEMA,
                kind: "snapshot".into(),
                root: "/r".into(),
                host: "h".into(),
                os: os.into(),
                scanned_at_ms: 0,
                duration_ms: 0,
                entry_count: entries.len() as u64,
                hashed: true,
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
    fn file(path: &str, hash: &str) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::File,
            size: 1,
            mtime_ms: 0,
            hash: Some(hash.into()),
            hash_failed: false,
            file_id: None,
            mode: None,
            link: None,
            prev: None,
        }
    }
    /// A file with an mtime (conflict arbitration goes by mtime)
    /// An archive entry: current hash + historic generations
    /// A snapshot of a VFS root: `header.os` carries the *protocol*, and the naming rules
    /// live in the VfsNote — exactly the shape `scan_vfs` writes.

    // P2-5: empty files / ambiguous pairing

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
    fn evidence_reports_both_sides_and_equal_count() {
        // same: identical on both sides; upd: on both sides but with different content; only_s: source only; only_t: target only
        let s = snap(
            "windows",
            vec![
                Entry {
                    size: 10,
                    mtime_ms: 1_000,
                    ..file("same.txt", "h0")
                },
                Entry {
                    size: 30,
                    mtime_ms: 9_000,
                    ..file("upd.txt", "hs")
                },
                Entry {
                    size: 7,
                    mtime_ms: 5_000,
                    ..file("only_s.txt", "h1")
                },
            ],
        );
        let t = snap(
            "windows",
            vec![
                Entry {
                    size: 10,
                    mtime_ms: 1_000,
                    ..file("same.txt", "h0")
                },
                Entry {
                    size: 20,
                    mtime_ms: 2_000,
                    ..file("upd.txt", "ht")
                },
                Entry {
                    size: 4,
                    mtime_ms: 3_000,
                    ..file("only_t.txt", "h2")
                },
            ],
        );
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let ev = evidence(&s, &t, &plan, &CompareOptions::default());
        assert_eq!(
            ev.metas.len(),
            plan.ops.len(),
            "the evidence array must correspond one-to-one with ops"
        );
        assert_eq!(ev.equal_count, 1);
        assert_eq!(ev.equal_bytes, 10);

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
    fn same_page_lists_only_equal_files_and_pages() {
        let mk = |n: usize, h: &str| Entry {
            size: n as u64,
            mtime_ms: n as i64 * 1000,
            ..file(&format!("d{}/f{n}.bin", n % 3), h)
        };
        let s = snap("windows", (0..10).map(|n| mk(n, "same")).collect());
        // The last 3 differ in content on the target side → not counted as equal
        let t = snap(
            "windows",
            (0..10)
                .map(|n| mk(n, if n >= 7 { "diff" } else { "same" }))
                .collect(),
        );
        let copts = CompareOptions::default();
        let (total, rows) = same_page(&s, &t, &copts, "", 0, 100);
        assert_eq!(total, 7);
        assert_eq!(rows.len(), 7);
        // Paging
        let (total, rows) = same_page(&s, &t, &copts, "", 5, 100);
        assert_eq!(
            total, 7,
            "total is the post-filter total, independent of the paging window"
        );
        assert_eq!(rows.len(), 2);
        let (_t, rows) = same_page(&s, &t, &copts, "", 0, 3);
        assert_eq!(rows.len(), 3);
        // Substring filter (case-insensitive)
        let (total, rows) = same_page(&s, &t, &copts, "D1/", 0, 100);
        assert_eq!(total as usize, rows.len());
        assert!(rows.iter().all(|r| r.path.starts_with("d1/")));
        // Both sides' times must be surfaced (identical content, timestamps may differ)
        assert!(rows.iter().all(|r| r.mtime_ms == r.other_mtime_ms));
    }

    #[test]
    fn evidence_follows_move_naming_on_each_side() {
        // Same-content rename: source is already called b.bin, target is still a.bin — each side is looked up under its own name
        let s = snap(
            "windows",
            vec![Entry {
                size: 42,
                mtime_ms: 8_000,
                ..file("b.bin", "hm")
            }],
        );
        let t = snap(
            "windows",
            vec![Entry {
                size: 42,
                mtime_ms: 4_000,
                ..file("a.bin", "hm")
            }],
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
