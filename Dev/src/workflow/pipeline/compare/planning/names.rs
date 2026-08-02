//! Destination-name collision and backend-legality safeguards.

use std::collections::HashMap;

use crate::foundation::names::windows_name_fault;
use crate::foundation::text::fold;
use crate::fs::vfs::NameRules;
use crate::model::plan::{Action, Op, Side};
use crate::model::table::TableArtifact;

use super::super::matching::name_rules::name_rules_of;

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
    for operation in operations {
        if !matches!(operation.action, Action::Copy | Action::Move) {
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
            }
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
        // Only Copy and Move create a new name. Update already addresses an existing entry.
        let creates = matches!(operation.action, Action::Copy | Action::Move);
        let reads_other = matches!(operation.action, Action::Copy | Action::Update);
        let candidates = [Some(operation.path.as_str()), operation.from.as_deref()];
        let mut verdict: Option<(bool, String)> = None;

        for path in candidates.into_iter().flatten() {
            let Some((fault, reason)) = windows_name_fault(path) else {
                continue;
            };
            let executing_hit = executing_rules == NameRules::Windows
                && (fault.changes_addressed_path() || creates);
            let reading_hit = reads_other
                && reading_rules == NameRules::Windows
                && fault.changes_addressed_path();
            if executing_hit || reading_hit {
                let location = if reading_hit && !executing_hit {
                    "reading side"
                } else {
                    "executing side"
                };
                verdict = Some((true, format!("{reason} ({location})")));
                break;
            }

            let executing_unknown = executing_rules == NameRules::Unknown
                && (fault.changes_addressed_path() || creates);
            let reading_unknown = reads_other
                && reading_rules == NameRules::Unknown
                && fault.changes_addressed_path();
            if (executing_unknown || reading_unknown) && verdict.is_none() {
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
