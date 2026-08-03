//! Desktop-facing compare evidence: per-row measurements, paged identical items, and operation
//! reversal.
//!
//! `compare()` never calls into this module, and `evidence()` / `identical_page()` only describe a plan
//! that already exists — neither can change what a sync does.
//!
//! `reverse_op` is executable semantics: the desktop previews the same transformation, while Rust
//! reconstructs the authenticated operation before apply.
//!
//! Three rules here are re-implemented in `Dev/typescript/core/domain/compare/plan.ts`, because the
//! window cannot ask the backend per keystroke for a six-figure table or per click for a direction
//! toggle. They are not equal in kind, and the distinction is the reason the mirror is allowed:
//!
//! - `reverse_op` is an **engine rule**. The frontend re-derives it for preview and totals only;
//!   Apply carries `{index, direction_reversed}` and Rust reconstructs the executed op from the
//!   authenticated plan. Run Scope membership and plan order stay the engine's.
//! - `side_paths` is **presentation** — two table columns, two CSV columns, and the path the File
//!   Manager is pointed at. Shortening a rendered path is the window's alone and has no owner here.
//! - `implied_row_meta` / `row_meta` are a **decoder** for a compression this crate's callers
//!   perform on the wire, not a second policy: `metas[i]` is elided exactly where the row's own
//!   fields reproduce it.
//!
//! Rust owns all three. `Dev/src/workflow/pipeline/compare/tests/rule_vectors.rs` emits the vectors
//! that hold the TypeScript copy to them, and `npm run gen:types` refuses to leave those vectors
//! stale.

use serde::{Deserialize, Serialize};

use crate::model::plan::{Action, Op, Plan, Side};
use crate::model::table::{ObservedEntry, ObservedEntryKind, TableArtifact};

use std::collections::BTreeMap;

use super::matching::{files_equal, map_of};
use super::CompareOptions;
use crate::foundation::text::norm_key;

/// One side's measured state at compare time. Display and sorting read it, and so does `reverse_op`:
/// a reversed content Update writes the bytes its new origin side was observed to hold, and this is
/// the only measurement that still exists once the row's own `size` describes the losing side.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub struct SideMeta {
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number")]
    pub mtime_ms: i64,
}

/// Measured state of both sides, one-to-one with `plan.ops[i]` (the absent side is None)
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
/// Per-side measurements remain parallel to `Plan.ops` rather than living on `Op`, which would
/// change the plan JSONL format and the CLI contract. Reversal is the one place where a measurement
/// becomes an executable field: `reverse_op` moves the new origin's `size` onto the reconstructed
/// op, because that row is executed and gated like any other. Identical totals use the comparison's
/// own key folding and equality predicate.
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
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
///
/// `metadata` is the row's retained per-side evidence, and it is what keeps a reversed Update
/// measurable: the bytes it writes are the ones its new origin side was observed to hold. A content
/// Update whose new origin was never measured refuses instead, because a sizeless write row is read
/// as zero bytes by `guard::stats` and skipped outright by the free-space gate.
pub fn reverse_op(operation: &Op, metadata: &RowMeta) -> Option<Op> {
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
        // The other side's content wins, so the row no longer describes *this* side's bytes: the
        // new origin is the side that was about to be written, and its measured size is what the
        // reversed row will write. A symlink op keeps its absent size — it publishes a link, not
        // content, and is never measured on either side. `hash` and `mtime_ms` belonged to the
        // side that just lost; `copy_file` re-stats the origin for the mtime it needs.
        Action::Update => {
            let size = if operation.link.is_some() {
                None
            } else {
                let new_origin = match operation.side {
                    Side::Source => metadata.src,
                    Side::Target => metadata.dst,
                };
                Some(new_origin?.size)
            };
            Some(Op {
                side: opposite_side,
                from: None,
                size,
                mtime_ms: None,
                hash: None,
                reason,
                ..operation.clone()
            })
        }
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

/// The retained entry a row's own fields already imply, or `None` when the row implies nothing.
///
/// A `Copy` row exists on exactly one side: the side it is written *to* has nothing to measure, and
/// the row already carries the originating side's `size`/`mtime_ms`. That makes its `metas` entry
/// redundant, and eliding it is what keeps a six-figure plan out of WebKit's retained heap.
///
/// This is the one statement of that redundancy. Publication elides an entry only where this
/// reproduces the measured evidence exactly, artifact validation accepts an elided entry only on the
/// same terms, and every reader rebuilds it through `row_meta`. A compressor that dropped more than
/// a decoder can rebuild would be a row silently displayed as unmeasured, so the three sites read
/// one function instead of spelling one predicate three times.
pub fn implied_row_meta(operation: &Op) -> Option<RowMeta> {
    if operation.action != Action::Copy {
        return None;
    }
    let (Some(size), Some(mtime_ms)) = (operation.size, operation.mtime_ms) else {
        return None;
    };
    let existing = Some(SideMeta { size, mtime_ms });
    Some(if operation.side == Side::Target {
        RowMeta {
            src: existing,
            dst: None,
        }
    } else {
        RowMeta {
            src: None,
            dst: existing,
        }
    })
}

/// A row's effective per-side evidence: what Compare retained, or what the row itself implies when
/// the retained entry was elided as redundant. Unmeasured on both sides is the honest answer for a
/// row that implies nothing — it is never invented from the other side.
pub fn row_meta(operation: &Op, retained: Option<&RowMeta>) -> RowMeta {
    retained
        .cloned()
        .or_else(|| implied_row_meta(operation))
        .unwrap_or_default()
}

/// The paths that exist on each side at compare time, before the operation is executed.
///
/// Copies and deletes have one present side. A move is still called `from` on the executing side and
/// already `path` on the other, so each side is named under its own spelling. Everything else is
/// present under one name on both sides.
///
/// Presentation: the CSV export writes these as two columns, File-Manager reveal picks one of them,
/// and the desktop table renders them. How a rendered path is *shortened* — a group basename, or
/// "(this folder)" — is the window's own and deliberately has no counterpart here.
pub fn side_paths(operation: &Op) -> (Option<&str>, Option<&str>) {
    let executes_on_target = operation.side == Side::Target;
    match operation.action {
        Action::Copy => {
            if executes_on_target {
                (Some(&operation.path), None)
            } else {
                (None, Some(&operation.path))
            }
        }
        Action::Move => {
            let current = operation.from.as_deref().unwrap_or(&operation.path);
            if executes_on_target {
                (Some(&operation.path), Some(current))
            } else {
                (Some(current), Some(&operation.path))
            }
        }
        Action::Delete | Action::DeleteDir => {
            if executes_on_target {
                (None, Some(&operation.path))
            } else {
                (Some(&operation.path), None)
            }
        }
        _ => (Some(&operation.path), Some(&operation.path)),
    }
}
