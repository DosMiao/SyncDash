//! In-process run orchestration.
//!
//! A local run is defined by who executes it, not by the root protocol: mounted paths and VFS
//! roots are handled here, while `peer://` work belongs to the peer lane. The tree separates the
//! read-only Compare pipeline, the guarded Apply boundary, and the CLI's multi-target execution
//! loop so that safety policy is not interleaved with presentation and iteration.

mod apply;
mod compare;
mod execute;

pub use apply::{
    apply_job_guarded_with, apply_job_guarded_with_classified, apply_requirements,
    apply_requirements_resolved, apply_resolved, apply_resolved_classified, preflight_job,
    preflight_resolved,
};
pub use compare::{compare_capabilities, compare_job_detailed, compare_resolved};
pub use execute::{run_local_job, run_local_single};

/// Emit the pre-run capability list, most-departing first.
///
/// Severity picks the log level so the list keeps its shape in a text log, where there are no
/// cards to carry it. It records what the run will do; it decides nothing.
pub(super) fn log_capability_list(
    ctx: &crate::obs::progress::RunCtx,
    scope: &str,
    report: &crate::pipeline::guard::caps::CapReport,
) {
    use crate::model::event::LogLevel;
    use crate::pipeline::guard::caps::CapSeverity;

    for item in report.listed() {
        let level = match item.severity {
            CapSeverity::Unavailable | CapSeverity::Degraded => LogLevel::Warn,
            CapSeverity::Info => LogLevel::Info,
        };
        ctx.log(level, scope, item.render());
    }
}

#[cfg(test)]
mod tests;
