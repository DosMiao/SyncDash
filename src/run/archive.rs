//! The sync-mode archive: the record of what the two sides last agreed on.
//!
//! Refreshed after a successful apply, and never over paths that ended in conflict — an archive
//! that claims agreement where there was none turns the next run's conflict into a silent
//! overwrite.

use crate::job::Job;
use crate::model::plan::{Action, Plan};
use crate::model::table::Snapshot;
use crate::pipeline::scan;

use super::roots::resolve_root;
use super::scan_opts;

/// Refresh the archive after a successful sync: rescan source, drop conflicted paths (a conflict keeps being reported, never silently arbitrated).
/// v0.9 M1: make the Refresh phase visible — the archive rescan is a long phase that is completely invisible today, so wire it to the event stream and cancellation.
/// Being cancelled only means conflicts get re-reported next round — safe.
pub fn refresh_archive_with(job: &Job, plan: &Plan, ctx: &crate::obs::progress::RunCtx) {
    let Some(arch_path) = &job.archive else {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "run",
            "hint: sync job without `archive` — add one so deletions/moves can be attributed next time",
        );
        return;
    };
    let conflicted: std::collections::HashSet<&str> = plan
        .ops
        .iter()
        .filter(|o| o.action == Action::Conflict)
        .map(|o| o.path.as_str())
        .collect();
    let opt = scan_opts(job);
    // The previous-generation archive: every row of the new table pushes the old hash onto the prev chain, so that
    // "one generation behind" can be told apart from "concurrent modification" (P1-3, see compare::generation_of)
    let previous = if arch_path.is_file() { Snapshot::load(arch_path).ok() } else { None };
    let Ok(svr) = resolve_root(&job.source) else { return };
    if let Ok(mut snap) = scan::scan_root(&svr, &opt, ctx, crate::model::event::Phase::Refresh) {
        snap.header.kind = "archive".into();
        snap.entries.retain(|e| !conflicted.contains(e.path.as_str()));
        if let Some(prev) = &previous {
            crate::model::table::roll_generations(&mut snap.entries, &prev.entries);
        }
        if let Some(dir) = arch_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(f) = std::fs::File::create(arch_path) {
            let mut w = std::io::BufWriter::new(f);
            if snap.write_to(&mut w).is_ok() {
                ctx.log(
                    crate::model::event::LogLevel::Info,
                    "run",
                    format!("archive refreshed: {}", arch_path.display()),
                );
            }
        }
    }
}
