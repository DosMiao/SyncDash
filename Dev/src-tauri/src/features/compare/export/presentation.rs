//! Exact row selection and direction projection for one retained Compare result.

use std::collections::HashSet;

use syncdash::model::plan::{Action, Op, Side};
use syncdash::pipeline::compare::evidence::{reverse_op, RowMeta, SideMeta};

use crate::contracts::compare::CsvRowPresentationDto;

#[derive(Debug)]
pub(crate) struct CsvRow {
    pub(crate) included: bool,
    pub(crate) operation: Op,
    pub(crate) metadata: RowMeta,
}

pub(crate) fn validated_rows(
    operations: &[Op],
    metadata: &[Option<RowMeta>],
    presentation: &[CsvRowPresentationDto],
) -> Result<Vec<CsvRow>, String> {
    let mut seen = HashSet::with_capacity(presentation.len());
    let mut rows = Vec::with_capacity(presentation.len());
    for row in presentation {
        if !seen.insert(row.index) {
            return Err(format!(
                "Export row {} was submitted more than once",
                row.index + 1
            ));
        }
        let original = operations.get(row.index).ok_or_else(|| {
            format!(
                "Export row {} is outside this Compare result",
                row.index + 1
            )
        })?;
        let operation = presented_operation(operations, row.index, row.direction_reversed)?;
        let metadata = metadata
            .get(row.index)
            .and_then(Clone::clone)
            .unwrap_or_else(|| metadata_from_operation(original));
        rows.push(CsvRow {
            included: row.included,
            operation,
            metadata,
        });
    }
    Ok(rows)
}

pub(crate) fn presented_operation(
    operations: &[Op],
    index: usize,
    direction_reversed: bool,
) -> Result<Op, String> {
    let operation = operations
        .get(index)
        .ok_or_else(|| format!("Compare row {} is outside this result", index + 1))?;
    if direction_reversed {
        reverse_op(operation).ok_or_else(|| format!("Compare row {} cannot be reversed", index + 1))
    } else {
        Ok(operation.clone())
    }
}

fn metadata_from_operation(operation: &Op) -> RowMeta {
    if operation.action == Action::Copy {
        if let (Some(size), Some(mtime_ms)) = (operation.size, operation.mtime_ms) {
            let existing = Some(SideMeta { size, mtime_ms });
            return if operation.side == Side::Target {
                RowMeta {
                    src: existing,
                    dst: None,
                }
            } else {
                RowMeta {
                    src: None,
                    dst: existing,
                }
            };
        }
    }
    RowMeta::default()
}

pub(crate) fn operation_side_paths(operation: &Op) -> (Option<&str>, Option<&str>) {
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

#[cfg(test)]
mod tests {
    use super::super::fixtures::operation;
    use super::*;

    #[test]
    fn row_injection_duplicates_and_invalid_reversals_are_rejected() {
        let operations = [operation(Action::Conflict, Side::Target, "conflict.txt")];
        let outside = [CsvRowPresentationDto {
            index: 1,
            included: true,
            direction_reversed: false,
        }];
        assert!(validated_rows(&operations, &[None], &outside)
            .unwrap_err()
            .contains("outside"));

        let duplicate = [
            CsvRowPresentationDto {
                index: 0,
                included: true,
                direction_reversed: false,
            },
            CsvRowPresentationDto {
                index: 0,
                included: false,
                direction_reversed: false,
            },
        ];
        assert!(validated_rows(&operations, &[None], &duplicate)
            .unwrap_err()
            .contains("more than once"));

        let reversed = [CsvRowPresentationDto {
            index: 0,
            included: true,
            direction_reversed: true,
        }];
        assert!(validated_rows(&operations, &[None], &reversed)
            .unwrap_err()
            .contains("cannot be reversed"));
    }
}
