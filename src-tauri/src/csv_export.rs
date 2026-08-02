//! CSV rendering from one exact retained Compare result.

use std::collections::HashSet;
use std::io::Write;

use serde::Serialize;
use syncdash::model::plan::{Action, Op, PlanHeader, Side};
use syncdash::pipeline::compare::evidence::{reverse_op, RowMeta, SideMeta};

use crate::dto::CsvRowPresentationDto;

#[derive(Debug)]
struct CsvRow {
    included: bool,
    operation: Op,
    metadata: RowMeta,
}

fn validated_rows(
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

fn safe_filename_component(value: &str) -> String {
    let mut component = String::new();
    for character in value.trim().chars().take(64) {
        if character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            if !component.ends_with('-') {
                component.push('-');
            }
        } else {
            component.push(character);
        }
    }
    component
        .trim_matches(|character| matches!(character, ' ' | '.' | '-'))
        .to_string()
}

pub(crate) fn default_export_filename(
    job_name: &str,
    compare_run_id: u64,
    generated_at_ms: u64,
) -> Result<String, String> {
    let job_component = safe_filename_component(job_name);
    if job_component.is_empty() {
        return Err("The retained Compare result has no usable job name for export".into());
    }
    let generated_at_ms = i64::try_from(generated_at_ms)
        .map_err(|_| "The export timestamp is outside the supported range".to_string())?;
    Ok(format!(
        "SyncDash-{job_component}-compare-{compare_run_id}-{}.csv",
        syncdash::foundation::time::stamp_compact(generated_at_ms)
    ))
}

fn full_path(root: &str, relative: Option<&str>) -> String {
    let Some(relative) = relative else {
        return String::new();
    };
    let separator = if root.contains('\\') { '\\' } else { '/' };
    let root = root.trim_end_matches(['/', '\\']);
    let relative = if separator == '\\' {
        relative.replace('/', "\\")
    } else {
        relative.to_string()
    };
    format!("{root}{separator}{relative}")
}

fn spreadsheet_safe(value: &str) -> String {
    if value
        .chars()
        .next()
        .is_some_and(|first| matches!(first, '=' | '+' | '-' | '@' | '\t' | '\r'))
    {
        format!("'{value}")
    } else {
        value.to_string()
    }
}

fn csv_string(value: &str) -> String {
    format!("\"{}\"", spreadsheet_safe(value).replace('"', "\"\""))
}

fn timestamp(milliseconds: i64) -> String {
    if milliseconds <= 0 {
        String::new()
    } else {
        syncdash::foundation::time::stamp_iso(milliseconds)
    }
}

fn json_token<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value)
        .map(|serialized| serialized.trim_matches('"').to_string())
        .map_err(|error| format!("Cannot serialize an export token: {error}"))
}

pub(crate) fn write_compare_csv(
    writer: &mut dyn Write,
    header: &PlanHeader,
    operations: &[Op],
    metadata: &[Option<RowMeta>],
    presentation: &[CsvRowPresentationDto],
) -> Result<usize, String> {
    let rows = validated_rows(operations, metadata, presentation)?;
    writer
        .write_all(&[0xEF, 0xBB, 0xBF])
        .map_err(|error| error.to_string())?;
    writeln!(writer, "included,action,side,rel_path,from,source_path,target_path,src_size,src_mtime_utc,dst_size,dst_mtime_utc,reason")
        .map_err(|error| error.to_string())?;
    for row in &rows {
        let operation = &row.operation;
        let (source_relative, target_relative) = operation_side_paths(operation);
        let source_path = full_path(&header.source_root, source_relative);
        let target_path = full_path(&header.target_root, target_relative);
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            u8::from(row.included),
            json_token(&operation.action)?,
            json_token(&operation.side)?,
            csv_string(&operation.path),
            csv_string(operation.from.as_deref().unwrap_or("")),
            csv_string(&source_path),
            csv_string(&target_path),
            row.metadata
                .src
                .map(|side| side.size.to_string())
                .unwrap_or_default(),
            row.metadata
                .src
                .map(|side| timestamp(side.mtime_ms))
                .unwrap_or_default(),
            row.metadata
                .dst
                .map(|side| side.size.to_string())
                .unwrap_or_default(),
            row.metadata
                .dst
                .map(|side| timestamp(side.mtime_ms))
                .unwrap_or_default(),
            csv_string(&operation.reason),
        )
        .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())?;
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(action: Action, side: Side, path: &str) -> Op {
        Op {
            side,
            action,
            path: path.into(),
            from: None,
            size: Some(10),
            mtime_ms: Some(1_700_000_000_000),
            hash: None,
            link: None,
            mode: None,
            reason: "reviewed".into(),
        }
    }

    fn header() -> PlanHeader {
        PlanHeader {
            source_root: "/source".into(),
            target_root: "/target".into(),
            ..serde_json::from_str(
                r#"{"schema":1,"kind":"plan","mode":"mirror","generated_at_ms":0,"source_root":"","source_host":"","target_root":"","target_host":"","op_count":0,"conflict_count":0,"source_entries":0,"target_entries":0,"source_excluded":0,"target_excluded":0,"source_walk_errors":0,"target_walk_errors":0,"source_walk_err_samples":[],"target_walk_err_samples":[]}"#,
            )
            .unwrap()
        }
    }

    #[test]
    fn exact_rows_are_validated_and_rendered_in_requested_order() {
        let operations = [
            operation(Action::Copy, Side::Target, "alpha.txt"),
            operation(Action::Delete, Side::Target, "beta.txt"),
        ];
        let presentation = [
            CsvRowPresentationDto {
                index: 1,
                included: false,
                direction_reversed: true,
            },
            CsvRowPresentationDto {
                index: 0,
                included: true,
                direction_reversed: false,
            },
        ];
        let mut output = Vec::new();
        let count = write_compare_csv(
            &mut output,
            &header(),
            &operations,
            &[None, None],
            &presentation,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(count, 2);
        assert!(text.find("beta.txt").unwrap() < text.find("alpha.txt").unwrap());
        assert!(text.contains("0,copy,source"));
        assert!(text.contains("1,copy,target"));
    }

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

    #[test]
    fn text_cells_are_neutralized_before_csv_escaping() {
        let mut dangerous = operation(Action::Copy, Side::Target, "=cmd|' /C calc'!A0");
        dangerous.reason = "+SUM(1,1)".into();
        let presentation = [CsvRowPresentationDto {
            index: 0,
            included: true,
            direction_reversed: false,
        }];
        let mut output = Vec::new();
        write_compare_csv(&mut output, &header(), &[dangerous], &[None], &presentation).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\"'=cmd|' /C calc'!A0\""));
        assert!(text.contains("\"'+SUM(1,1)\""));
    }

    #[test]
    fn default_filename_is_cross_platform_safe_and_run_specific() {
        let filename =
            default_export_filename(" Photos: 2026 / Primary? ", 42, 1_700_000_000_000).unwrap();
        assert_eq!(
            filename,
            "SyncDash-Photos- 2026 - Primary-compare-42-20231114-221320.csv"
        );
        assert!(!filename.chars().any(|character| matches!(
            character,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
        )));
    }
}
