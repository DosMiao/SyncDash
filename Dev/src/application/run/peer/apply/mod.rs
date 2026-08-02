//! Peer apply facade: validate the reviewed subset, execute it, and classify the mutation boundary.

mod classification;
mod execution;
mod policy;

use crate::job::SingleTargetJob;
use crate::model::plan::{Op, Plan};

pub(super) use self::classification::classify_peer_completion;
use self::execution::apply_peer_inner;
use super::controller::emit_apply_summary;

pub use policy::{apply_capabilities, preflight_peer_job};

/// Apply a reviewed peer-plan subset and return its terminal counters.
pub fn apply_peer_job_with(
    name: &str,
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<crate::obs::progress::ApplyOutcome> {
    apply_peer_job_with_classified(name, job, plan, ops, verbose, acknowledged, ctx).into_result()
}

pub fn apply_peer_job_with_classified(
    name: &str,
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::run::ApplyExecution {
    let t0 = std::time::Instant::now();
    let mut writes_started = false;
    let mut r = apply_peer_inner(
        name,
        job,
        plan,
        ops,
        verbose,
        acknowledged,
        ctx,
        &mut writes_started,
    );
    if let Ok(out) = &mut r {
        out.cancelled |= ctx.ctl.cancelled();
    }
    let terminal = match &r {
        Ok(out) => *out,
        Err(e) => crate::obs::progress::ApplyOutcome {
            errors: u64::from(!crate::obs::progress::is_cancelled(e)),
            cancelled: crate::obs::progress::is_cancelled(e) || ctx.ctl.cancelled(),
            ..Default::default()
        },
    };
    emit_apply_summary(ctx, t0, terminal);
    classify_peer_completion(writes_started, r)
}
