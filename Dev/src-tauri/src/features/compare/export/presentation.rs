//! Exact row selection and direction projection for one retained Compare result.

use std::collections::HashSet;

use syncdash::model::plan::Op;
use syncdash::pipeline::compare::evidence::{reverse_op, row_meta, RowMeta};

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
        let evidence = row_meta(original, metadata.get(row.index).and_then(Option::as_ref));
        let operation = directed_operation(original, &evidence, row.index, row.direction_reversed)?;
        rows.push(CsvRow {
            included: row.included,
            operation,
            metadata: evidence,
        });
    }
    Ok(rows)
}

pub(crate) fn presented_operation(
    operations: &[Op],
    metadata: &[Option<RowMeta>],
    index: usize,
    direction_reversed: bool,
) -> Result<Op, String> {
    let operation = operations
        .get(index)
        .ok_or_else(|| format!("Compare row {} is outside this result", index + 1))?;
    let evidence = row_meta(operation, metadata.get(index).and_then(Option::as_ref));
    directed_operation(operation, &evidence, index, direction_reversed)
}

/// One row projected in the direction the operator is looking at it. Reversal reads the row's
/// evidence, so export, reveal, and the authenticated Apply reconstruct the identical operation.
fn directed_operation(
    operation: &Op,
    metadata: &RowMeta,
    index: usize,
    direction_reversed: bool,
) -> Result<Op, String> {
    if direction_reversed {
        reverse_op(operation, metadata)
            .ok_or_else(|| format!("Compare row {} cannot be reversed", index + 1))
    } else {
        Ok(operation.clone())
    }
}

#[cfg(test)]
mod tests {
    use syncdash::model::plan::{Action, Side};

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
