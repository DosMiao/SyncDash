//! `syncdash logs` — listing runs and reading back their artifacts.
//!
//! The widest rendering in the CLI, and the reason it is here rather than inline in the dispatch:
//! column layout and age wording are presentation, and the library returns records.

use syncdash::obs::runlog;

use super::args::LogsCmd;

/// v0.10: the CLI facade for central logging. Every read goes through runlog's public API,
/// which is the layer holding the path-escape guard (`artifact_lines` accepts only bare-filename-shaped run_ids).
pub fn run_logs(cmd: LogsCmd) -> std::io::Result<i32> {
    match cmd {
        LogsCmd::List { job, limit } => {
            // history_merged rather than history: an interrupted run has only a directory and no index line,
            // and that is precisely the run that most needs to be seen
            let rows = runlog::history_merged(job.as_deref(), limit);
            if rows.is_empty() {
                println!("no runs recorded yet (runs are logged when a job actually applies)");
                return Ok(0);
            }
            let now = syncdash::foundation::time::now_ms() as i64;
            for r in &rows {
                let age_min = (now - r.ts_ms).max(0) / 60_000;
                let age = if age_min < 60 {
                    format!("{age_min}m ago")
                } else if age_min < 48 * 60 {
                    format!("{}h ago", age_min / 60)
                } else {
                    format!("{}d ago", age_min / 60 / 24)
                };
                // compare rows have no directory; "-" holds the slot so the column stays visually aligned
                let state = if !r.finished {
                    "  [INTERRUPTED]"
                } else if r.cancelled {
                    "  [cancelled]"
                } else {
                    ""
                };
                let what = match r.ops_found {
                    Some(n) => format!("{n:>5} found"),
                    None => format!("{:>5} done ", r.done),
                };
                println!(
                    "{:>9}  {:<28} {:<16} {:<12} {what} {:>3} err {:>3} warn  {:>10}  {:>7.1}s{state}",
                    age,
                    r.run_id.as_deref().unwrap_or("-"),
                    r.job,
                    r.kind,
                    r.errors,
                    r.warnings,
                    syncdash::foundation::fmt::human_bytes(r.bytes),
                    r.elapsed_ms as f64 / 1000.0,
                );
            }
            println!(
                "\n{} run(s) · logs at {}",
                rows.len(),
                runlog::logs_dir().display()
            );
            Ok(0)
        }
        LogsCmd::Show {
            run_id,
            errors,
            items,
            plan,
            limit,
        } => {
            let which = if errors {
                "errors"
            } else if items {
                "items"
            } else if plan {
                "plan"
            } else {
                "run"
            };
            let lines = runlog::artifact_lines(&run_id, which, limit);
            if lines.is_empty() {
                eprintln!(
                    "no {which} lines for run '{run_id}' (wrong id, or that artifact is empty)"
                );
                return Ok(1);
            }
            for l in lines {
                println!("{l}");
            }
            Ok(0)
        }
        LogsCmd::Prune {
            keep_days,
            max_total_mb,
        } => {
            let cfg = syncdash::store::settings::load();
            let days = keep_days.unwrap_or(cfg.keep_days);
            let cap = max_total_mb.unwrap_or(cfg.max_total_mb);
            let n = runlog::prune(days, cap);
            println!("pruned {n} run(s)  (keep_days={days}, max_total_mb={cap})");
            Ok(0)
        }
        LogsCmd::Dir => {
            println!("{}", runlog::logs_dir().display());
            println!(
                "settings: {}",
                syncdash::store::settings::settings_path().display()
            );
            Ok(0)
        }
    }
}
