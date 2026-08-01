//! Reading a finished compare: the identical-items page, and CSV export.

use serde::Serialize;
use std::sync::Arc;

use syncdash::model::plan::{Op, PlanHeader};
use syncdash::pipeline::compare;
use syncdash::job;

use crate::dto::{CompareOwner, SamePage};
use crate::state::{resolve_target, validate_cached_compare, SnapCache};

/// Pagination for the "Identical" panel. The data source is the snapshot left by the last compare — no rescan.
#[tauri::command]
pub fn list_same(
    snaps: tauri::State<'_, Arc<SnapCache>>,
    owner: CompareOwner,
    query: String,
    offset: usize,
    limit: usize,
) -> Result<SamePage, String> {
    let (job_name, full_job) = job::load_named(&owner.job_name).map_err(|e| e.to_string())?;
    let config_revision =
        job::config_revision(&full_job).map_err(|e| format!("Job '{job_name}': {e}"))?;
    let (target_index, job) = resolve_target(&full_job, Some(owner.target_index))?;
    let g = snaps.0.lock().unwrap();
    validate_cached_compare(
        g.as_ref().map(|c| &c.provenance),
        &owner,
        &job_name,
        target_index,
        &config_revision,
        None,
    )?;
    let c = g.as_ref().expect("successful cache validation requires a cached comparison");
    let (total, rows) = compare::evidence::same_page(&c.source, &c.target, &job.compare_opts(), &query, offset, limit.min(2000));
    Ok(SamePage { total, rows, job: job_name })
}

/// Export the current view as CSV. Escaping happens exactly once, here, and the output is UTF-8 **with a BOM** —
/// without the BOM Excel interprets the file in the local code page and non-ASCII paths turn into mojibake.
#[tauri::command]
pub fn export_csv(
    path: String,
    header: PlanHeader,
    ops: Vec<Op>,
    metas: Vec<compare::evidence::RowMeta>,
    checked: Vec<bool>,
) -> Result<usize, String> {
    use std::io::Write;
    fn esc(s: &str) -> String {
        if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    }
    // An absent mtime is a blank cell, never 1970. The local timezone offset is the UI's job;
    // this writes ISO UTC so cross-machine reconciliation is unambiguous.
    fn stamp(ms: i64) -> String {
        if ms <= 0 { String::new() } else { syncdash::foundation::time::stamp_iso(ms) }
    }
    let f = std::fs::File::create(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut w = std::io::BufWriter::new(f);
    w.write_all(&[0xEF, 0xBB, 0xBF]).map_err(|e| e.to_string())?;
    writeln!(w, "checked,action,side,rel_path,from,source_path,target_path,src_size,src_mtime_utc,dst_size,dst_mtime_utc,reason")
        .map_err(|e| e.to_string())?;
    let sep = if header.target_root.contains('\\') { '\\' } else { '/' };
    let join = |root: &str, rel: &str| {
        let r = root.trim_end_matches(['/', '\\']);
        let rel = if sep == '\\' { rel.replace('/', "\\") } else { rel.to_string() };
        format!("{r}{sep}{rel}")
    };
    // Action/side use serde's snake_case form (see json_token), matching the literals in the plan JSONL
    // and the event stream — Debug's PascalCase would leave the CSV out of step with every other output
    for (i, op) in ops.iter().enumerate() {
        let m = metas.get(i).cloned().unwrap_or_default();
        let on = checked.get(i).copied().unwrap_or(false);
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            if on { 1 } else { 0 },
            json_token(&op.action),
            json_token(&op.side),
            esc(&op.path),
            esc(op.from.as_deref().unwrap_or("")),
            esc(&join(&header.source_root, &op.path)),
            esc(&join(&header.target_root, &op.path)),
            m.src.map(|s| s.size.to_string()).unwrap_or_default(),
            m.src.map(|s| stamp(s.mtime_ms)).unwrap_or_default(),
            m.dst.map(|s| s.size.to_string()).unwrap_or_default(),
            m.dst.map(|s| stamp(s.mtime_ms)).unwrap_or_default(),
            esc(&op.reason),
        )
        .map_err(|e| e.to_string())?;
    }
    w.flush().map_err(|e| e.to_string())?;
    Ok(ops.len())
}

/// The public literals for enums follow serde (Action/Side are both marked rename_all = "snake_case"),
/// so delete_dir in the CSV is the same word the plan JSONL and the event stream write.
fn json_token<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default().trim_matches('"').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use syncdash::model::plan::PlanHeader;
    use syncdash::model::plan::{Action, Side};
use syncdash::pipeline::compare::evidence::SideMeta;

    #[test]
    fn csv_escapes_commas_and_quotes_and_carries_both_sides() {
        let dir = std::env::temp_dir().join("syncdash-csv-test");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("plan.csv");
        let header = PlanHeader {
            schema: syncdash::model::plan::PLAN_SCHEMA, kind: "plan".into(), mode: "mirror".into(), generated_at_ms: 0,
            source_root: r"D:\S".into(), source_host: "h".into(),
            target_root: r"E:\T".into(), target_host: "h".into(),
            op_count: 1, conflict_count: 0, source_entries: 1, target_entries: 1,
            source_excluded: 0, target_excluded: 0,
            source_walk_errors: 0, target_walk_errors: 0,
            source_walk_err_samples: Vec::new(), target_walk_err_samples: Vec::new(),
            source_icloud_stubs: 0, target_icloud_stubs: 0,
            source_icloud_stub_samples: Vec::new(), target_icloud_stub_samples: Vec::new(),
        };
        let ops = vec![Op {
            side: Side::Target,
            action: Action::DeleteDir,
            // Both a comma and a double quote in the path — the two classic CSV landmines
            path: "b/y,z\"q.txt".into(),
            from: None, size: Some(20), mtime_ms: None, hash: None, link: None, mode: None,
            reason: "gone, really".into(),
        }];
        // mtime_ms = 0 means "no time available" in a snapshot (scan writes 0 when it cannot read metadata),
        // so use real non-zero times here instead of pressing the epoch into service as a date
        let metas = vec![compare::evidence::RowMeta {
            src: Some(SideMeta { size: 10, mtime_ms: 86_400_000 }),
            dst: Some(SideMeta { size: 20, mtime_ms: 172_800_000 }),
        }];
        let n = export_csv(out.display().to_string(), header, ops, metas, vec![true]).unwrap();
        assert_eq!(n, 1);
        let bytes = std::fs::read(&out).unwrap();
        // Excel does not recognize UTF-8 without a BOM; non-ASCII paths turn the whole column into mojibake
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
        let row = text.lines().nth(1).unwrap();
        assert!(row.contains("\"b/y,z\"\"q.txt\""), "commas and quotes in the path must be escaped per RFC4180: {row}");
        assert!(row.contains("\"gone, really\""));
        // Enum literals share their source with the plan JSONL (snake_case), not Debug's PascalCase
        assert!(row.contains(",delete_dir,target,"), "enums should be snake_case: {row}");
        // Both sides' size/time must be written out
        assert!(row.contains(",10,1970-01-02T00:00:00Z,20,1970-01-03T00:00:00Z,"), "{row}");
        // The absent side leaves empty columns — no zero fill, no fabricated date
        let one_sided = vec![compare::evidence::RowMeta { src: None, dst: Some(SideMeta { size: 5, mtime_ms: 86_400_000 }) }];
        let ops2 = vec![Op {
            side: Side::Target, action: Action::Delete, path: "x".into(), from: None,
            size: Some(5), mtime_ms: None, hash: None, link: None, mode: None, reason: "gone".into(),
        }];
        let h2 = PlanHeader {
            schema: syncdash::model::plan::PLAN_SCHEMA, kind: "plan".into(), mode: "mirror".into(), generated_at_ms: 0,
            source_root: "/s".into(), source_host: "h".into(),
            target_root: "/t".into(), target_host: "h".into(),
            op_count: 1, conflict_count: 0, source_entries: 1, target_entries: 1,
            source_excluded: 0, target_excluded: 0,
            source_walk_errors: 0, target_walk_errors: 0,
            source_walk_err_samples: Vec::new(), target_walk_err_samples: Vec::new(),
            source_icloud_stubs: 0, target_icloud_stubs: 0,
            source_icloud_stub_samples: Vec::new(), target_icloud_stub_samples: Vec::new(),
        };
        export_csv(out.display().to_string(), h2, ops2, one_sided, vec![false]).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let row = text.lines().nth(1).unwrap();
        assert!(row.starts_with("0,delete,target,x,,/s/x,/t/x,,,5,1970-01-02T00:00:00Z,gone"), "{row}");
        let _ = std::fs::remove_file(&out);
    }
}
