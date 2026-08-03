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

use super::matching::{files_equal, map_of};
use super::CompareOptions;
use crate::foundation::text::norm_key;

/// One side's measured state at compare time. **For display and sorting only** — apply never reads a single byte of it.
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
