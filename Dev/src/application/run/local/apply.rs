//! Apply authorization, preflight, and the write-start classification boundary.

use crate::job::{Job, SingleTargetJob};
use crate::model::plan::{Op, Plan};
use crate::pipeline::apply;

use super::super::archive::refresh_archive_with;
use super::super::roots::resolve_root;

/// Run the gates without executing — the GUI calls this before raising the confirmation sheet, so
/// refusal reasons are shown in full instead of landing only on an unseen stderr stream.
pub fn preflight_job(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    acknowledged: bool,
) -> std::io::Result<crate::pipeline::guard::Verdict> {
    let configuration = job.configuration();
    let source = resolve_root(&configuration.source)?;
    let target = resolve_root(job.target())?;
    Ok(preflight_resolved(
        configuration,
        plan,
        ops,
        acknowledged,
        &source,
        &target,
    ))
}

pub fn apply_requirements(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    acknowledged: bool,
) -> std::io::Result<super::super::ApplyRequirements> {
    let configuration = job.configuration();
    let source = resolve_root(&configuration.source)?;
    let target = resolve_root(job.target())?;
    Ok(apply_requirements_resolved(
        configuration,
        plan,
        ops,
        acknowledged,
        &source,
        &target,
    ))
}

pub fn apply_requirements_resolved(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    acknowledged: bool,
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
) -> super::super::ApplyRequirements {
    let verdict = preflight_resolved(job, plan, ops, acknowledged, source, target);
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
    acknowledged: bool,
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
) -> crate::pipeline::guard::Verdict {
    let mut verdict = crate::pipeline::guard::Verdict {
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
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
    crate::pipeline::guard::run_all_vfs(
        ops,
        source,
        target,
        &plan.header,
        &job.guards(acknowledged),
    )
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

/// Every apply invocation has one terminal event, including failures before `Phase::Apply` can
/// start. The progress window derives its running state from this boundary; without it a refused
/// run keeps spinning after the engine has already released the run slot.
pub(super) fn finish_apply(
    ctx: &crate::obs::progress::RunCtx,
    t0: std::time::Instant,
    mut out: crate::obs::progress::ApplyOutcome,
) -> crate::obs::progress::ApplyOutcome {
    out.cancelled |= ctx.ctl.cancelled();
    ctx.sink.emit(crate::model::event::ProgressEvent::Summary {
        ts_ms: crate::foundation::time::now_ms(),
        done: out.done,
        skipped: out.skipped,
        errors: out.errors,
        bytes_done: out.bytes_copied,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        paused_ms: ctx.ctl.paused_total_ms(),
        cancelled: out.cancelled,
    });
    out
}

pub fn apply_job_guarded_with(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    apply_job_guarded_with_consent(
        job,
        plan,
        ops,
        trash,
        verbose,
        acknowledged,
        &crate::pipeline::guard::caps::CapabilityConsent::explicit_cli(accept_caps),
        ctx,
    )
}

#[allow(clippy::too_many_arguments)] // each argument is an independently reviewed apply decision
pub fn apply_job_guarded_with_consent(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    apply_job_guarded_with_consent_classified(
        job,
        plan,
        ops,
        trash,
        verbose,
        acknowledged,
        consent,
        ctx,
    )
    .into_result()
    .expect("the local apply lane represents every refusal as an ApplyOutcome")
}

#[allow(clippy::too_many_arguments)] // each argument is an independently reviewed apply decision
pub fn apply_job_guarded_with_consent_classified(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
    ctx: &crate::obs::progress::RunCtx,
) -> super::super::ApplyExecution {
    let t0 = std::time::Instant::now();
    let configuration = job.configuration();
    let sv = match resolve_root(&configuration.source) {
        Ok(v) => v,
        Err(e) => {
            return super::super::ApplyExecution::rejected(finish_apply(
                ctx,
                t0,
                refuse_apply(ctx, ops.len(), "resolve-roots", e.to_string()),
            ))
        }
    };
    let tv = match resolve_root(job.target()) {
        Ok(v) => v,
        Err(e) => {
            return super::super::ApplyExecution::rejected(finish_apply(
                ctx,
                t0,
                refuse_apply(ctx, ops.len(), "resolve-roots", e.to_string()),
            ))
        }
    };
    apply_resolved_with_consent_classified(
        configuration,
        plan,
        ops,
        &sv,
        &tv,
        trash,
        verbose,
        acknowledged,
        consent,
        t0,
        ctx,
    )
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
    acknowledged: bool,
    accept_caps: bool,
    t0: std::time::Instant,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    apply_resolved_with_consent(
        job,
        plan,
        ops,
        sv,
        tv,
        trash,
        verbose,
        acknowledged,
        &crate::pipeline::guard::caps::CapabilityConsent::explicit_cli(accept_caps),
        t0,
        ctx,
    )
}

#[allow(clippy::too_many_arguments)] // resolved roots and apply decisions remain explicit at this boundary
pub fn apply_resolved_with_consent(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
    t0: std::time::Instant,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    apply_resolved_with_consent_classified(
        job,
        plan,
        ops,
        sv,
        tv,
        trash,
        verbose,
        acknowledged,
        consent,
        t0,
        ctx,
    )
    .into_result()
    .expect("the local apply lane represents every refusal as an ApplyOutcome")
}

#[allow(clippy::too_many_arguments)] // resolved roots and apply decisions remain explicit at this boundary
pub fn apply_resolved_with_consent_classified(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
    t0: std::time::Instant,
    ctx: &crate::obs::progress::RunCtx,
) -> super::super::ApplyExecution {
    use crate::model::event::{Phase, ProgressEvent};
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
        return super::super::ApplyExecution::rejected(finish_apply(ctx, t0, out));
    }
    // Write-side capability report: gaps listed BEFORE anything is touched
    {
        use crate::model::event::LogLevel;
        use crate::pipeline::guard::caps::CapSeverity;
        let q = job.write_caps_query(sv.local_root().is_some(), tv.local_root().is_some());
        let wr = crate::pipeline::guard::caps::cap_report_write(&q, ops, &sv.caps(), &tv.caps());
        for i in &wr.items {
            let lvl = match i.severity {
                CapSeverity::Block => LogLevel::Error,
                CapSeverity::NeedsAck => LogLevel::Warn,
                CapSeverity::Info => LogLevel::Info,
            };
            ctx.log(lvl, "caps", i.render());
        }
        let blockers = wr.blockers();
        if !blockers.is_empty() {
            let out = refuse_apply(
                ctx,
                ops.len(),
                "caps",
                blockers
                    .iter()
                    .map(|i| i.render())
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            return super::super::ApplyExecution::rejected(finish_apply(ctx, t0, out));
        }
        let acks = wr.needs_ack();
        if !wr.consent_satisfied(
            crate::pipeline::guard::caps::CapabilityScope::ApplyWrite,
            consent,
        ) {
            let instruction = match consent {
                crate::pipeline::guard::caps::CapabilityConsent::ExactDigest(_) => {
                    "the capability report changed after it was authorized — review Apply again"
                }
                _ => "rerun with --accept-caps to consent",
            };
            let out =
                refuse_apply(
                    ctx,
                    ops.len(),
                    "caps",
                    format!(
                    "this apply degrades on capabilities the backends lack — {instruction}:\n  {}",
                    acks.iter().map(|i| i.render()).collect::<Vec<_>>().join("\n  ")
                ),
                );
            return super::super::ApplyExecution::rejected(finish_apply(ctx, t0, out));
        }
    }
    let verdict =
        crate::pipeline::guard::run_all_vfs(ops, sv, tv, &plan.header, &job.guards(acknowledged));
    if !verdict.report("preflight") {
        for b in &verdict.blockers {
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::foundation::time::now_ms(),
                path: String::new(),
                action: "preflight".into(),
                side: "target".into(),
                message: b.clone(),
            });
        }
        let out = ApplyOutcome {
            done: 0,
            skipped: ops.len() as u64,
            errors: 1,
            bytes_copied: 0,
            cancelled: false,
        };
        return super::super::ApplyExecution::rejected(finish_apply(ctx, t0, out));
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
    super::super::ApplyExecution::started(finish_apply(ctx, t0, out))
}
