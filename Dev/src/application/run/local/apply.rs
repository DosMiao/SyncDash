//! Apply authorization, preflight, and the write-start classification boundary.

use crate::job::{Job, SingleTargetJob};
use crate::model::plan::{Op, Plan};
use crate::pipeline::apply;

use super::super::archive::refresh_archive_with;
use super::super::roots::resolve_root;

pub fn apply_requirements(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
) -> std::io::Result<super::super::ApplyRequirements> {
    let configuration = job.configuration();
    let source = resolve_root(&configuration.source)?;
    let target = resolve_root(job.target())?;
    Ok(apply_requirements_resolved(
        configuration,
        plan,
        ops,
        &source,
        &target,
    ))
}

pub fn apply_requirements_resolved(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
) -> super::super::ApplyRequirements {
    let verdict = preflight_resolved(job, plan, ops, source, target);
    let query = job.write_caps_query(source.local_root().is_some(), target.local_root().is_some());
    let capabilities =
        crate::pipeline::guard::caps::cap_report_write(&query, ops, &source.caps(), &target.caps());
    super::super::ApplyRequirements {
        verdict,
        capabilities,
    }
}

pub fn preflight_resolved(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
) -> crate::pipeline::guard::Verdict {
    let mut verdict = crate::pipeline::guard::Verdict::default();
    let source_label = root_label(source);
    let target_label = root_label(target);
    if source_label != plan.header.source_root || target_label != plan.header.target_root {
        verdict.blockers.push(format!(
            "this plan was made for '{}' → '{}' but the job resolves to '{}' → '{}' — run Compare again",
            plan.header.source_root,
            plan.header.target_root,
            source_label,
            target_label,
        ));
        return verdict;
    }
    crate::pipeline::guard::run_all_vfs(ops, source, target, &plan.header, &job.guards())
}

fn root_label(vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>) -> String {
    vfs.local_root()
        .map(|root| root.display_path().to_string_lossy().into_owned())
        .unwrap_or_else(|| vfs.display())
}

/// The refusal shape both halves of the apply gate share: one `Error` event, and an outcome that did
/// nothing. A module-level fn rather than a closure because the phrase half (a root that will not
/// open) and the backend half (a capability report that blocks) both have to raise it.
fn refuse_apply(
    ctx: &crate::obs::progress::RunCtx,
    ops_len: usize,
    action: &str,
    message: String,
) -> crate::obs::progress::ApplyOutcome {
    use crate::model::event::{Phase, ProgressEvent};
    ctx.sink.emit(ProgressEvent::Error {
        phase: Phase::Apply,
        ts_ms: crate::foundation::time::now_ms(),
        path: String::new(),
        action: action.into(),
        side: "target".into(),
        message,
    });
    crate::obs::progress::ApplyOutcome {
        done: 0,
        skipped: ops_len as u64,
        errors: 1,
        bytes_copied: 0,
        cancelled: false,
    }
}

pub fn apply_job_guarded_with(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    apply_job_guarded_with_classified(job, plan, ops, trash, verbose, ctx)
        .into_result()
        .expect("the local apply lane represents every refusal as an ApplyOutcome")
}

pub fn apply_job_guarded_with_classified(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> super::super::ApplyExecution {
    let t0 = std::time::Instant::now();
    let configuration = job.configuration();
    let sv = match resolve_root(&configuration.source) {
        Ok(v) => v,
        Err(e) => {
            return super::super::ApplyExecution::rejected(
                refuse_apply(ctx, ops.len(), "resolve-roots", e.to_string()).finish(ctx, t0),
            )
        }
    };
    let tv = match resolve_root(job.target()) {
        Ok(v) => v,
        Err(e) => {
            return super::super::ApplyExecution::rejected(
                refuse_apply(ctx, ops.len(), "resolve-roots", e.to_string()).finish(ctx, t0),
            )
        }
    };
    apply_resolved_classified(configuration, plan, ops, &sv, &tv, trash, verbose, t0, ctx)
}

/// Apply a plan to two roots that are already open. Split out from `apply_job_guarded_with` the same
/// way, and for the same reason, `compare_resolved` was split from `compare_job_detailed`:
/// everything below here works on backends, not spellings — which is what lets the write lane be
/// exercised against an in-memory root instead of only against a phrase naming a real disk.
///
/// `t0` belongs to the caller, so the Summary still measures from before the roots were opened.
#[allow(clippy::too_many_arguments)] // every one is a distinct decision the caller has already made
pub fn apply_resolved(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    t0: std::time::Instant,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    apply_resolved_classified(job, plan, ops, sv, tv, trash, verbose, t0, ctx)
        .into_result()
        .expect("the local apply lane represents every refusal as an ApplyOutcome")
}

#[allow(clippy::too_many_arguments)] // resolved roots and apply decisions remain explicit at this boundary
pub fn apply_resolved_classified(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    t0: std::time::Instant,
    ctx: &crate::obs::progress::RunCtx,
) -> super::super::ApplyExecution {
    use crate::obs::progress::ApplyOutcome;
    // The plan must be the one made for THESE roots. The header carries the label the
    // scan wrote: the local (possibly translated) path for local lanes, the display
    // phrase for generic-lane roots.
    if root_label(sv) != plan.header.source_root || root_label(tv) != plan.header.target_root {
        let out = refuse_apply(
            ctx,
            ops.len(),
            "resolve-roots",
            format!(
                "this plan was made for '{}' → '{}' but the job resolves to '{}' → '{}' — run compare again",
                plan.header.source_root,
                plan.header.target_root,
                root_label(sv),
                root_label(tv)
            ),
        );
        return super::super::ApplyExecution::rejected(out.finish(ctx, t0));
    }
    // Write-side capability report: every gap is listed BEFORE anything is touched, so a run that
    // departs from what the job asked for says so first. The list does not hold the run back; a
    // capability the backend truly lacks surfaces as the failure of the operation that needs it.
    {
        let q = job.write_caps_query(sv.local_root().is_some(), tv.local_root().is_some());
        let wr = crate::pipeline::guard::caps::cap_report_write(&q, ops, &sv.caps(), &tv.caps());
        super::log_capability_list(ctx, "caps", &wr);
    }
    let verdict = crate::pipeline::guard::run_all_vfs(ops, sv, tv, &plan.header, &job.guards());
    // Every warning reaches the run's own event stream, not only the process log. A deletion share
    // or an incomplete scan is now the whole signal rather than the preamble to a refusal, so a
    // reader of this run's transcript has to see it without going looking somewhere else.
    for warning in &verdict.warnings {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "preflight",
            warning.clone(),
        );
    }
    if !verdict.report("preflight") {
        let out = super::super::refused_by_preflight(ctx, &verdict.blockers, ops.len());
        return super::super::ApplyExecution::rejected(out.finish(ctx, t0));
    }
    let ap = apply::apply_vfs(ops, sv, tv, &job.apply_opts(trash, verbose), ctx);
    let mut out = ApplyOutcome {
        cancelled: ctx.ctl.cancelled(),
        ..ap
    };
    // A cancelled run does not refresh the archive: the user asked to "stop now", and re-reporting conflicts next round is safe anyway
    if out.errors == 0 && !out.cancelled && job.mode == "sync" {
        let refreshed = refresh_archive_with(
            job,
            plan,
            sv,
            &super::super::effective_scan_opts(job, sv, tv),
            ctx,
        );
        out.cancelled = ctx.ctl.cancelled();
        if !refreshed && !out.cancelled {
            out.errors += 1;
        }
    }
    super::super::ApplyExecution::started(out.finish(ctx, t0))
}
