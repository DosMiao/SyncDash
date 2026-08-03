//! Destination-name collision and backend-legality safeguards.

use std::collections::HashMap;

use crate::foundation::names::windows_name_fault;
use crate::foundation::text::fold;
use crate::fs::vfs::NameRules;
use crate::model::plan::{Action, Op, Side};
use crate::model::table::TableArtifact;

use super::super::super::name_safety::{hazard_sides, NameRoles};
use super::super::matching::name_rules::name_rules_of;

/// Copy and Move are the actions that bring a *new* name into existence on the side they run on.
/// Update writes a name the destination snapshot already holds, so it is covered by the existing-
/// entry branch below rather than by the planned-write grouping.
fn creates_a_destination_name(operation: &Op) -> bool {
    matches!(operation.action, Action::Copy | Action::Move)
}

pub(super) fn reject_case_collisions(
    source: &TableArtifact,
    target: &TableArtifact,
    case_insensitive: bool,
    operations: &mut [Op],
) {
    if case_insensitive {
        return;
    }
    let mut folded: HashMap<(bool, String), Vec<&str>> = HashMap::new();
    for (is_target, snapshot) in [(false, source), (true, target)] {
        for entry in &snapshot.entries {
            let path = entry.path().as_str();
            folded
                .entry((is_target, fold(path)))
                .or_default()
                .push(path);
        }
    }

    // Two planned writes can fold onto each other with neither destination present in any
    // snapshot, and that case is invisible to the entry map above: on a case-insensitive
    // destination the later write lands on the earlier one and the run reports N successes for one
    // surviving file. Grouping the planned destinations against each other is the only place that
    // can see it — apply-side validation keys on the exact byte path, so the twins look distinct
    // there, and by then the plan is already authorized.
    //
    // Every member of a folding group is refused, not all-but-one. Allowing one through would make
    // the engine pick a winner out of plan order, which is arbitrary and destroys the losers'
    // bytes without ever saying so; refusing the whole group loses nothing and names the choice
    // the user has to make.
    let mut groups: HashMap<(bool, String), Vec<usize>> = HashMap::new();
    for (index, operation) in operations.iter().enumerate() {
        if creates_a_destination_name(operation) {
            groups
                .entry((operation.side == Side::Target, fold(&operation.path)))
                .or_default()
                .push(index);
        }
    }
    let mut collisions: Vec<Option<String>> = vec![None; operations.len()];
    for indexes in groups.into_values() {
        if indexes.len() < 2 {
            continue;
        }
        for &index in &indexes {
            let others = indexes
                .iter()
                .filter(|&&other| other != index)
                .map(|&other| format!("'{}'", operations[other].path))
                .collect::<Vec<_>>()
                .join(", ");
            collisions[index] = Some(format!(
                "case-collision: writing '{}' and {others} would leave a single file on a \
                 case-insensitive filesystem, so every one of them is refused rather than \
                 picking a winner (set case_sensitive = false, or rename one side)",
                operations[index].path
            ));
        }
    }

    for (index, operation) in operations.iter_mut().enumerate() {
        if !creates_a_destination_name(operation) {
            continue;
        }
        let is_target = operation.side == Side::Target;
        if let Some(existing) = folded.get(&(is_target, fold(&operation.path))) {
            let from = operation.from.as_deref();
            if let Some(other) = existing
                .iter()
                .find(|path| **path != operation.path && Some(**path) != from)
            {
                operation.action = Action::Conflict;
                operation.reason = format!(
                    "case-collision: writing '{}' would overwrite existing '{other}' on a \
                     case-insensitive filesystem (set case_sensitive = false, or rename one side)",
                    operation.path
                );
                continue;
            }
        }
        if let Some(reason) = collisions[index].take() {
            operation.action = Action::Conflict;
            operation.reason = reason;
        }
    }
}

pub(super) fn validate_backend_legality(
    source: &TableArtifact,
    target: &TableArtifact,
    operations: &mut Vec<Op>,
) {
    let mut unknown_rule_notes: Vec<(Side, String, String)> = Vec::new();
    for operation in operations.iter_mut() {
        let (executing_rules, reading_rules) = match operation.side {
            Side::Target => (name_rules_of(&target.header), name_rules_of(&source.header)),
            Side::Source => (name_rules_of(&source.header), name_rules_of(&target.header)),
        };
        let roles = NameRoles::of(&operation.action);
        let candidates = [Some(operation.path.as_str()), operation.from.as_deref()];
        let mut verdict: Option<(bool, String)> = None;

        for path in candidates.into_iter().flatten() {
            let Some((fault, reason)) = windows_name_fault(path) else {
                continue;
            };
            let windows = hazard_sides(
                fault,
                roles,
                executing_rules,
                reading_rules,
                NameRules::Windows,
            );
            if windows.any() {
                let location = if windows.reading && !windows.executing {
                    "reading side"
                } else {
                    "executing side"
                };
                verdict = Some((true, format!("{reason} ({location})")));
                break;
            }

            let unknown = hazard_sides(
                fault,
                roles,
                executing_rules,
                reading_rules,
                NameRules::Unknown,
            );
            if unknown.any() && verdict.is_none() {
                verdict = Some((false, reason));
            }
        }

        match verdict {
            Some((true, reason)) => {
                operation.action = Action::Conflict;
                operation.reason = format!("illegal-on-windows: {reason}");
            }
            Some((false, reason)) => {
                unknown_rule_notes.push((operation.side.clone(), operation.path.clone(), reason))
            }
            None => {}
        }
    }

    for (side, path, reason) in unknown_rule_notes {
        operations.push(Op {
            side,
            action: Action::Note,
            path,
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: format!(
                "name-risk-on-unknown-server: {reason} — this root's OS cannot be determined over its protocol, so the operation is attempted as planned"
            ),
        });
    }
}
