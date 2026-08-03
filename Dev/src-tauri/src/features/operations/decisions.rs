//! Reconstruction of executable operations from authenticated review decisions.

use syncdash::model::plan::Op;
use syncdash::pipeline::compare::evidence::RowMeta;

use crate::contracts::compare::ReviewedRowDecisionDto;

/// `plan_metadata` is the retained per-side evidence of the same Compare result, one entry per
/// operation. A reversal is reconstructed from it, so an evidence array that does not cover the
/// plan cannot authenticate a reversed row and is refused here rather than downgraded.
pub(crate) fn resolve_reviewed_operations(
    plan_operations: &[Op],
    plan_metadata: &[Option<RowMeta>],
    reviewed_row_decisions: &[ReviewedRowDecisionDto],
) -> Result<Vec<Op>, String> {
    if reviewed_row_decisions.is_empty() {
        return Err(
            "No executable differences were included — review this Compare result first".into(),
        );
    }
    if plan_metadata.len() != plan_operations.len() {
        return Err(
            "This Compare result's row evidence does not cover its plan — run Compare again".into(),
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
            let metadata = plan_metadata[index].clone().unwrap_or_default();
            syncdash::pipeline::compare::evidence::reverse_op(original, &metadata).ok_or_else(
                || {
                    format!(
                        "Reviewed row {} cannot be reversed — run Compare again",
                        index + 1
                    )
                },
            )?
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
    use syncdash::pipeline::compare::evidence::SideMeta;

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

    /// What Compare retains for a row present on both sides.
    fn both_sides_measured(source_size: u64, target_size: u64) -> Option<RowMeta> {
        Some(RowMeta {
            src: Some(SideMeta {
                size: source_size,
                mtime_ms: 34,
            }),
            dst: Some(SideMeta {
                size: target_size,
                mtime_ms: 56,
            }),
        })
    }

    #[test]
    fn reviewed_row_decisions_reconstruct_operations_and_valid_reversals() {
        let plan = [
            operation(Action::Copy, "safe/file.txt"),
            operation(Action::Update, "other.txt"),
        ];
        let metadata = [None, both_sides_measured(12, 900)];
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
        let resolved =
            resolve_reviewed_operations(&plan, &metadata, &reviewed_row_decisions).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].path, "safe/file.txt");
        assert!(matches!(resolved[0].action, Action::Delete));
        assert_eq!(resolved[1].path, "other.txt");
        assert!(matches!(resolved[1].action, Action::Update));
    }

    /// The reconstruction is what preflight and execution judge, so a reversed Update has to arrive
    /// carrying the bytes it will write — taken from the side it now reads, not the side it now
    /// overwrites. A sizeless write row is one the free-space gate skips.
    #[test]
    fn a_reversed_update_is_reconstructed_with_the_bytes_its_new_origin_holds() {
        let plan = [operation(Action::Update, "other.txt")];
        let metadata = [both_sides_measured(12, 900)];
        let resolved = resolve_reviewed_operations(
            &plan,
            &metadata,
            &[ReviewedRowDecisionDto {
                index: 0,
                direction_reversed: true,
            }],
        )
        .unwrap();
        assert_eq!(resolved[0].side, Side::Source);
        assert_eq!(resolved[0].size, Some(900));
    }

    /// Fail closed rather than reconstruct an unmeasurable write: retained evidence that does not
    /// cover the plan, or a row whose new origin was never measured, sends the operator back to
    /// Compare instead of into a run no gate can judge.
    #[test]
    fn a_reversal_without_usable_row_evidence_is_refused() {
        let plan = [operation(Action::Update, "other.txt")];
        let reverse_first_row = [ReviewedRowDecisionDto {
            index: 0,
            direction_reversed: true,
        }];

        assert!(resolve_reviewed_operations(&plan, &[], &reverse_first_row)
            .unwrap_err()
            .contains("does not cover its plan"));
        assert!(
            resolve_reviewed_operations(&plan, &[None], &reverse_first_row)
                .unwrap_err()
                .contains("cannot be reversed")
        );
    }

    #[test]
    fn reviewed_row_decisions_reject_injection_duplicates_and_reports() {
        let plan = [
            operation(Action::Copy, "safe/file.txt"),
            operation(Action::Conflict, "conflict.txt"),
        ];
        let metadata = [None, None];

        assert!(resolve_reviewed_operations(&plan, &metadata, &[])
            .unwrap_err()
            .contains("No executable differences"));
        assert!(resolve_reviewed_operations(
            &plan,
            &metadata,
            &[ReviewedRowDecisionDto {
                index: 2,
                direction_reversed: false,
            }],
        )
        .unwrap_err()
        .contains("outside"));
        assert!(resolve_reviewed_operations(
            &plan,
            &metadata,
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
            &metadata,
            &[ReviewedRowDecisionDto {
                index: 1,
                direction_reversed: false,
            }],
        )
        .unwrap_err()
        .contains("not an operation"));
    }
}
