pub fn run_peer_job(
    name: &str,
    job: &SingleTargetJob,
    do_apply: bool,
    verbose: bool,
) -> std::io::Result<(u64, u64, u64, u64)> {
    run_peer_job_with(
        name,
        job,
        do_apply,
        verbose,
        &crate::obs::progress::RunCtx::null(),
    )
}

/// Keep the CLI's combined Compare/Apply flow on the same progress contract as the desktop's
/// separate commands, including a terminal Summary when Compare is cancelled.
pub fn run_peer_job_with(
    name: &str,
    job: &SingleTargetJob,
    do_apply: bool,
    verbose: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<(u64, u64, u64, u64)> {
    let plan = match crate::run::compare_peer_job_with(name, job, ctx) {
        Ok(p) => p,
        Err(e) => {
            // Cancelled in the compare stage: the terminal state must still be visible (the desktop closes out on Summary)
            if crate::obs::progress::is_cancelled(&e) {
                emit_cancel_summary(ctx, std::time::Instant::now());
            }
            return Err(e);
        }
    };
    ctx.log(
        crate::model::event::LogLevel::Info,
        "run",
        format!(
            "[{name}] {} op(s), {} conflict(s)  (peer pipeline via ssh)",
            plan.header.op_count, plan.header.conflict_count
        ),
    );
    crate::run::execute_planned_run(name, job, &plan, do_apply, ctx, |ops, run_ctx| {
        apply_peer_job_with(name, job, &plan, ops, verbose, run_ctx)
    })
}

fn emit_cancel_summary(ctx: &crate::obs::progress::RunCtx, t0: std::time::Instant) {
    crate::obs::progress::ApplyOutcome {
        cancelled: true,
        ..Default::default()
    }
    .finish(ctx, t0);
}

use crate::job::SingleTargetJob;

use super::apply::apply_peer_job_with;
