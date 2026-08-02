//! CSV serialization for already validated Compare row presentations.

use std::io::Write;

use serde::Serialize;
use syncdash::model::plan::{Op, PlanHeader};
use syncdash::pipeline::compare::evidence::RowMeta;

use crate::contracts::compare::CsvRowPresentationDto;

use super::presentation::{operation_side_paths, validated_rows};

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
    use syncdash::model::plan::{Action, Side};

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
}
