//! `syncdash logs` — listing runs and reading back their artifacts.
//!
//! The widest rendering in the CLI, and the reason it is here rather than inline in the dispatch:
//! column layout is presentation, and the library returns records.

use syncdash::run::history;

use crate::cli::args::LogsCmd;

/// Every read goes through runlog's descriptor-confined API; the CLI never constructs an artifact
/// path from a run identifier.
pub(super) fn run_logs(cmd: LogsCmd) -> std::io::Result<i32> {
    match cmd {
        LogsCmd::List { job, limit } => {
            // history_merged rather than history: an interrupted run has only a directory and no index line,
            // and that is precisely the run that most needs to be seen
            let rows = history::history_merged(job.as_deref(), limit)?;
            if rows.is_empty() {
                println!("{}", super::NO_RUNS_RECORDED);
                return Ok(0);
            }
            let now = syncdash::foundation::time::now_ms() as i64;
            for r in &rows {
                let age = super::relative_age(now, r.ts_ms);
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
                    "{:>9}  {:<32} {:<16} {:<12} {what} {:>3} err {:>3} warn  {:>10}  {:>7.1}s{state}",
                    age,
                    r.record_id,
                    r.subject.job_name,
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
                history::logs_dir().display()
            );
            Ok(0)
        }
        LogsCmd::Show {
            record_id,
            errors,
            items,
            plan,
            limit,
        } => {
            let (artifact, label) = if errors {
                (history::LogArtifactKind::Errors, "errors")
            } else if items {
                (history::LogArtifactKind::Items, "items")
            } else if plan {
                (history::LogArtifactKind::Plan, "plan")
            } else {
                (history::LogArtifactKind::Run, "run")
            };
            let lines = match history::artifact_lines(&record_id, artifact, limit) {
                Ok(lines) => lines,
                Err(error) => {
                    eprintln!("cannot read {label} for run '{record_id}': {error}");
                    return Ok(1);
                }
            };
            if lines.is_empty() {
                eprintln!("no {label} lines for run '{record_id}' (the artifact is empty)");
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
            let n = history::prune(days, cap)?;
            println!("pruned {n} run(s)  (keep_days={days}, max_total_mb={cap})");
            Ok(0)
        }
        LogsCmd::Dir => {
            println!("{}", history::logs_dir().display());
            println!(
                "settings: {}",
                syncdash::store::settings::settings_path().display()
            );
            Ok(0)
        }
    }
}
