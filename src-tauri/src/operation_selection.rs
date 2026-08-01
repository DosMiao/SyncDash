//! Reconstruction of executable operations from authenticated row decisions.

use syncdash::job;
use syncdash::model::plan::{Action, Op};

use crate::dto::SelectedRowDto;

/// Resolve a multi-target job to the engine's single-target view and preserve the normalized
/// index used by result and authorization identities.
pub(crate) fn resolve_target(
    job: &job::Job,
    target_index: Option<usize>,
) -> Result<(usize, job::Job), String> {
    job.validate_multi_target()?;
    let targets = job.target_list();
    let index = target_index.unwrap_or(0);
    let target = targets.get(index).ok_or_else(|| {
        format!(
            "target index {index} is out of range ({} total)",
            targets.len()
        )
    })?;
    Ok((index, job.for_target(target)))
}

pub(crate) fn resolve_selected_operations(
    plan_operations: &[Op],
    selected_rows: &[SelectedRowDto],
) -> Result<Vec<Op>, String> {
    if selected_rows.is_empty() {
        return Err("No executable rows were selected — review this Compare result first".into());
    }
    let mut decisions = vec![None; plan_operations.len()];
    for row in selected_rows {
        if row.index >= plan_operations.len() {
            return Err(format!(
                "Selected row {} is outside this Compare result — run Compare again",
                row.index + 1
            ));
        }
        if decisions[row.index].is_some() {
            return Err(format!(
                "Selected row {} was submitted more than once — run Compare again",
                row.index + 1
            ));
        }
        decisions[row.index] = Some(row.flipped);
    }

    let mut operations = Vec::with_capacity(selected_rows.len());
    for (index, flipped) in decisions.into_iter().enumerate() {
        let Some(flipped) = flipped else { continue };
        let original = &plan_operations[index];
        let operation = if flipped {
            syncdash::pipeline::compare::evidence::reverse_op(original).ok_or_else(|| {
                format!(
                    "Selected row {} cannot be reversed — run Compare again",
                    index + 1
                )
            })?
        } else {
            if matches!(original.action, Action::Conflict | Action::Note) {
                return Err(format!(
                    "Selected row {} is a report, not an operation — run Compare again",
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
    use syncdash::model::plan::Side;

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
    fn selected_rows_reconstruct_plan_operations_and_valid_reversals() {
        let plan = [
            operation(Action::Copy, "safe/file.txt"),
            operation(Action::Update, "other.txt"),
        ];
        let selected = [
            SelectedRowDto {
                index: 1,
                flipped: false,
            },
            SelectedRowDto {
                index: 0,
                flipped: true,
            },
        ];
        let resolved = resolve_selected_operations(&plan, &selected).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].path, "safe/file.txt");
        assert!(matches!(resolved[0].action, Action::Delete));
        assert_eq!(resolved[1].path, "other.txt");
        assert!(matches!(resolved[1].action, Action::Update));
    }

    #[test]
    fn selected_rows_reject_injection_duplicates_and_reports() {
        let plan = [
            operation(Action::Copy, "safe/file.txt"),
            operation(Action::Conflict, "conflict.txt"),
        ];

        assert!(resolve_selected_operations(&plan, &[])
            .unwrap_err()
            .contains("No executable rows"));
        assert!(resolve_selected_operations(
            &plan,
            &[SelectedRowDto {
                index: 2,
                flipped: false,
            }],
        )
        .unwrap_err()
        .contains("outside"));
        assert!(resolve_selected_operations(
            &plan,
            &[
                SelectedRowDto {
                    index: 0,
                    flipped: false,
                },
                SelectedRowDto {
                    index: 0,
                    flipped: true,
                },
            ],
        )
        .unwrap_err()
        .contains("more than once"));
        assert!(resolve_selected_operations(
            &plan,
            &[SelectedRowDto {
                index: 1,
                flipped: false,
            }],
        )
        .unwrap_err()
        .contains("not an operation"));
    }
}
