//! Rewriting the archive after a successful apply.
//!
//! Conflicted paths are excluded, always. An archive that claims agreement where there was none
//! turns the next run's conflict into a silent overwrite — which is the failure this whole module
//! exists to prevent, and the reason a refresh is skipped entirely when the apply did not fully
//! succeed.

use super::publish::*;
use super::target::ArchiveTarget;
use crate::job::Job;
use crate::model::plan::{Action, Plan};
use crate::pipeline::scan;

/// Refreshes the archive from the already-open source after a successful sync.
///
/// Conflicted paths remain absent so the next run reports them again. `opt` must be the effective
/// comparison options because archive digests are only comparable within the same evidence tier.
pub fn refresh_archive_with(
    job: &Job,
    plan: &Plan,
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &scan::ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
) -> bool {
    let Some(arch_path) = &job.archive else {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "run",
            "hint: sync job without `archive` — add one so deletions/moves can be attributed next time",
        );
        return true;
    };
    let conflicted: std::collections::HashSet<&str> = plan
        .ops
        .iter()
        .filter(|o| o.action == Action::Conflict)
        .map(|o| o.path.as_str())
        .collect();
    let mut snap = match scan::scan_root(sv, opt, ctx, crate::model::event::Phase::Refresh) {
        Ok(snap) => snap,
        Err(e) => {
            if crate::obs::progress::is_cancelled(&e) && ctx.ctl.cancelled() {
                return false;
            }
            ctx.sink.emit(crate::model::event::ProgressEvent::Error {
                phase: crate::model::event::Phase::Refresh,
                ts_ms: crate::foundation::time::now_ms(),
                path: arch_path.display().to_string(),
                action: "archive-scan".into(),
                side: "source".into(),
                message: e.to_string(),
            });
            return false;
        }
    };
    let saved = (|| -> std::io::Result<()> {
        let pp = crate::obs::progress::PhaseProgress::begin(
            ctx,
            crate::model::event::Phase::Archive,
            Some(arch_path.display().to_string()),
            1,
            0,
        );
        pp.checkpoint()?;
        // The generation chain distinguishes one-generation lag from concurrent modification.
        let archive = ArchiveTarget::open_for_write(arch_path)?;
        let lock = archive.acquire_lock()?;
        let previous = archive.load_or_migrate(&lock)?;
        snap.header.kind = crate::model::table::TableKind::Archive;
        snap.entries
            .retain(|entry| !conflicted.contains(entry.path().as_str()));
        snap.header.entry_count = snap.entries.len() as u64;
        if let Some(prev) = &previous {
            crate::model::table::roll_generations(&mut snap.entries, &prev.entries);
        }
        write_archive_to(
            &archive,
            &lock,
            |writer| snap.write_to(writer),
            || pp.checkpoint(),
        )?;
        ctx.log(
            crate::model::event::LogLevel::Info,
            "run",
            format!("archive refreshed: {}", arch_path.display()),
        );
        pp.item_done(&arch_path.display().to_string());
        pp.finish()?;
        Ok(())
    })();
    if let Err(e) = saved {
        if crate::obs::progress::is_cancelled(&e) && ctx.ctl.cancelled() {
            return false;
        }
        ctx.sink.emit(crate::model::event::ProgressEvent::Error {
            phase: crate::model::event::Phase::Archive,
            ts_ms: crate::foundation::time::now_ms(),
            path: arch_path.display().to_string(),
            action: "archive".into(),
            side: "source".into(),
            message: e.to_string(),
        });
        return false;
    }
    !ctx.ctl.cancelled()
}
