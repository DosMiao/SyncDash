//! Last-boundary validation for imported and locally generated plans.

use crate::foundation::names::windows_name_fault;
use crate::fs::vfs::NameRules;
use crate::model::plan::{Action, Op, Side};

/// Plans can come from files or another process, so every executable relative path is validated at
/// the last common boundary before any backend is opened. The same boundary rejects traversal,
/// platform prefixes, and SyncDash's reserved metadata namespace for every backend.
fn mutates_path(op: &Op) -> bool {
    matches!(
        op.action,
        Action::Copy | Action::Update | Action::Move | Action::Delete | Action::Chmod
    )
}

fn is_valid_move_identity_digest(hash: &str) -> bool {
    let digest = hash.strip_prefix('~').unwrap_or(hash);
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_ordered_move_source_recreation(ops: &[Op], moving: &Op, mutation: &Op) -> bool {
    // `Update` always reads this same relative path from `other`, i.e. the opposite root. The
    // scheduler runs the whole Move class before Update regardless of serialized plan order, so
    // this is a deliberate consume-then-recreate chain rather than two writers racing one name.
    // Reasons are deliberately excluded: imported plans must not gain authority by spoofing text.
    moving.action == Action::Move
        && mutation.action == Action::Update
        && moving.side == mutation.side
        && moving.from.as_deref() == Some(mutation.path.as_str())
        && mutation.from.is_none()
        && ops
            .iter()
            .filter(|candidate| {
                candidate.action == Action::Move
                    && candidate.side == moving.side
                    && candidate.from == moving.from
            })
            .count()
            == 1
        && ops
            .iter()
            .filter(|candidate| {
                mutates_path(candidate)
                    && candidate.side == mutation.side
                    && candidate.path == mutation.path
            })
            .count()
            == 1
}

pub(super) fn validate_operation_paths(ops: &[Op]) -> Result<(), String> {
    let mut mutations = std::collections::HashSet::new();
    let mut move_sources = std::collections::HashSet::new();
    for op in ops.iter().filter(|op| op.action.is_executable()) {
        let operation_path = crate::foundation::path::RootRelativePath::try_from(op.path.as_str())
            .map_err(|_| format!("unsafe operation path: {}", op.path))?;
        if crate::foundation::names::is_internal_artifact_path(&operation_path) {
            return Err(format!(
                "operation path is reserved for SyncDash metadata: {}",
                op.path
            ));
        }
        if matches!(op.action, Action::Move) {
            let from = op
                .from
                .as_deref()
                .ok_or_else(|| format!("move operation is missing its source path: {}", op.path))?;
            let source_path = crate::foundation::path::RootRelativePath::try_from(from)
                .map_err(|_| format!("unsafe move source path: {from}"))?;
            if crate::foundation::names::is_internal_artifact_path(&source_path) {
                return Err(format!(
                    "move source path is reserved for SyncDash metadata: {from}"
                ));
            }
            if from == op.path {
                return Err(format!("move source and destination are identical: {from}"));
            }
            if op.size.is_none() {
                return Err(format!(
                    "move operation has no source-size evidence: {from}"
                ));
            }
            if op.hash.is_none() && op.mtime_ms.is_none() && op.link.is_none() {
                return Err(format!(
                    "move operation has neither content, mtime, nor symlink-target evidence: {from}"
                ));
            }
            if op
                .hash
                .as_deref()
                .is_some_and(|hash| !is_valid_move_identity_digest(hash))
            {
                return Err(format!(
                    "move operation has an invalid content digest: {from}"
                ));
            }
            if op.link.is_some() && op.hash.is_some() {
                return Err(format!(
                    "move operation ambiguously carries both file and symlink evidence: {from}"
                ));
            }
            let source_key = (op.side == Side::Source, from);
            if !move_sources.insert(source_key) {
                return Err(format!(
                    "duplicate move source for {} path: {from}",
                    if op.side == Side::Source {
                        "source"
                    } else {
                        "target"
                    }
                ));
            }
            let planned_recreation = ops.iter().any(|mutation| {
                mutation.path == from && is_ordered_move_source_recreation(ops, op, mutation)
            });
            if mutations.contains(&source_key) && !planned_recreation {
                return Err(format!(
                    "move source is also mutated by another operation on the same side: {from}"
                ));
            }
        }
        if mutates_path(op) {
            let mutation_key = (op.side == Side::Source, op.path.as_str());
            let planned_recreation = ops
                .iter()
                .any(|moving| is_ordered_move_source_recreation(ops, moving, op));
            if move_sources.contains(&mutation_key) && !planned_recreation {
                return Err(format!(
                    "move source is also mutated by another operation on the same side: {}",
                    op.path
                ));
            }
            if !mutations.insert(mutation_key) {
                return Err(format!(
                    "duplicate mutation for {} path: {}",
                    if op.side == Side::Source {
                        "source"
                    } else {
                        "target"
                    },
                    op.path
                ));
            }
        }
    }
    Ok(())
}

/// Imported plans can bypass comparison, so known Windows naming semantics are enforced again at
/// the last boundary that sees both the operation and the actual backends it will touch.
pub(super) fn validate_operation_name_rules(
    ops: &[Op],
    source_rules: NameRules,
    target_rules: NameRules,
) -> Result<(), String> {
    for operation in ops
        .iter()
        .filter(|operation| operation.action.is_executable())
    {
        let (executing_rules, reading_rules) = match operation.side {
            Side::Source => (source_rules, target_rules),
            Side::Target => (target_rules, source_rules),
        };
        let creates_name = matches!(operation.action, Action::Copy | Action::Move);
        let reads_other_root = matches!(operation.action, Action::Copy | Action::Update);

        for path in [Some(operation.path.as_str()), operation.from.as_deref()]
            .into_iter()
            .flatten()
        {
            let Some((fault, reason)) = windows_name_fault(path) else {
                continue;
            };
            let unsafe_on_executing_root = executing_rules == NameRules::Windows
                && (fault.changes_addressed_path() || creates_name);
            let unsafe_on_reading_root = reads_other_root
                && reading_rules == NameRules::Windows
                && fault.changes_addressed_path();
            if unsafe_on_executing_root || unsafe_on_reading_root {
                let affected_root = match (unsafe_on_executing_root, unsafe_on_reading_root) {
                    (true, true) => "executing and reading",
                    (true, false) => "executing",
                    (false, true) => "reading",
                    (false, false) => unreachable!(),
                };
                return Err(format!(
                    "operation path {path:?} is unsafe on the {affected_root} root: {reason}"
                ));
            }
        }
    }
    Ok(())
}
