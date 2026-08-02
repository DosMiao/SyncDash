//! Reconstruction of executable operations from authenticated review decisions.

use syncdash::model::plan::Op;

use crate::contracts::compare::ReviewedRowDecisionDto;

pub(crate) fn resolve_reviewed_operations(
    plan_operations: &[Op],
    reviewed_row_decisions: &[ReviewedRowDecisionDto],
) -> Result<Vec<Op>, String> {
    if reviewed_row_decisions.is_empty() {
        return Err(
            "No executable differences were included — review this Compare result first".into(),
        );
    }
    let mut decisions = vec![None; plan_operations.len()];
    for decision in reviewed_row_decisions {
        if decision.index >= plan_operations.len() {
            return Err(format!(
                "Reviewed row {} is outside this Compare result — run Compare again",
                decision.index + 1
            ));
        }
        if decisions[decision.index].is_some() {
            return Err(format!(
                "Reviewed row {} was submitted more than once — run Compare again",
                decision.index + 1
            ));
        }
        decisions[decision.index] = Some(decision.direction_reversed);
    }

    let mut operations = Vec::with_capacity(reviewed_row_decisions.len());
    for (index, direction_reversed) in decisions.into_iter().enumerate() {
        let Some(direction_reversed) = direction_reversed else {
            continue;
        };
        let original = &plan_operations[index];
        let operation = if direction_reversed {
            syncdash::pipeline::compare::evidence::reverse_op(original).ok_or_else(|| {
                format!(
                    "Reviewed row {} cannot be reversed — run Compare again",
                    index + 1
                )
            })?
        } else {
            if !original.action.is_executable() {
                return Err(format!(
                    "Reviewed row {} is a report, not an operation — run Compare again",
                    index + 1
                ));
            }
            original.clone()
        };
        operations.push(operation);
    }
    Ok(operations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use syncdash::model::plan::{Action, Side};

    fn operation(action: Action, path: &str) -> Op {
        Op {
            side: Side::Target,
            action,
            path: path.into(),
            from: None,
            size: Some(12),
            mtime_ms: Some(34),
            hash: Some("hash".into()),
            link: None,
            mode: None,
            reason: "compared".into(),
        }
    }

    #[test]
    fn reviewed_row_decisions_reconstruct_operations_and_valid_reversals() {
        let plan = [
            operation(Action::Copy, "safe/file.txt"),
            operation(Action::Update, "other.txt"),
        ];
        let reviewed_row_decisions = [
            ReviewedRowDecisionDto {
                index: 1,
                direction_reversed: false,
            },
            ReviewedRowDecisionDto {
                index: 0,
                direction_reversed: true,
            },
        ];
        let resolved = resolve_reviewed_operations(&plan, &reviewed_row_decisions).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].path, "safe/file.txt");
        assert!(matches!(resolved[0].action, Action::Delete));
        assert_eq!(resolved[1].path, "other.txt");
        assert!(matches!(resolved[1].action, Action::Update));
    }

    #[test]
    fn reviewed_row_decisions_reject_injection_duplicates_and_reports() {
        let plan = [
            operation(Action::Copy, "safe/file.txt"),
            operation(Action::Conflict, "conflict.txt"),
        ];

        assert!(resolve_reviewed_operations(&plan, &[])
            .unwrap_err()
            .contains("No executable differences"));
        assert!(resolve_reviewed_operations(
            &plan,
            &[ReviewedRowDecisionDto {
                index: 2,
                direction_reversed: false,
            }],
        )
        .unwrap_err()
        .contains("outside"));
        assert!(resolve_reviewed_operations(
            &plan,
            &[
                ReviewedRowDecisionDto {
                    index: 0,
                    direction_reversed: false,
                },
                ReviewedRowDecisionDto {
                    index: 0,
                    direction_reversed: true,
                },
            ],
        )
        .unwrap_err()
        .contains("more than once"));
        assert!(resolve_reviewed_operations(
            &plan,
            &[ReviewedRowDecisionDto {
                index: 1,
                direction_reversed: false,
            }],
        )
        .unwrap_err()
        .contains("not an operation"));
    }
}
