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
#[allow(clippy::too_many_arguments)] // each argument is an independently reviewed run decision
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
    for op in &plan.ops {
        println!("{}", serde_json::to_string(op)?);
    }
    if !do_apply {
        println!("dry-run (rerun with --apply)");
        return Ok((0, plan.ops.len() as u64, 0, plan.header.conflict_count));
    }
    let ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| o.action.is_executable())
        .cloned()
        .collect();
    let t0 = std::time::Instant::now();
    let rec = crate::run::history::Recorder::start(
        crate::run::history::RunSubject::for_job(name, job),
        crate::run::apply_run_kind(job),
        ctx,
        &ops,
    );
    let out = apply_peer_job_with(name, job, &plan, &ops, verbose, &rec.ctx)?;
    let _ = rec.finish(&out, t0.elapsed().as_millis() as u64);
    Ok((
        out.done,
        out.skipped,
        out.errors,
        plan.header.conflict_count,
    ))
}

fn emit_cancel_summary(ctx: &crate::obs::progress::RunCtx, t0: std::time::Instant) {
    crate::obs::progress::ApplyOutcome {
        cancelled: true,
        ..Default::default()
    }
    .finish(ctx, t0);
}

use crate::job::SingleTargetJob;
use crate::model::plan::Op;

use super::apply::apply_peer_job_with;
