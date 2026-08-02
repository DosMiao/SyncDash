//! CLI-facing iteration over targets and one complete local Compare/Apply run.

use crate::job::{Job, SingleTargetJob};
use crate::model::plan::Op;

use super::{apply_job_guarded_with, compare_job_detailed};

/// End-to-end run for local or mounted-disk jobs. Returns `(done, skipped, errors, conflicts)`.
pub fn run_local_job(
    name: &str,
    job: &Job,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
) -> std::io::Result<(u64, u64, u64, u64)> {
    // 1:N (the original requirement): one source → each target compared and executed independently.
    // One plan and one run log per target; source-side hashing is absorbed by the cache (in the fast tier, near-zero reads from the second target on).
    job.validate()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let multi = job.targets.len() > 1;
    let mut tot = (0u64, 0u64, 0u64, 0u64);
    for (target_index, target_root) in job.targets.iter().enumerate() {
        let selected = job
            .select_target(target_index)
            .map_err(|reason| std::io::Error::new(std::io::ErrorKind::InvalidInput, reason))?;
        let label = if multi {
            format!(
                "{name}[{}/{} → {target_root}]",
                target_index + 1,
                job.targets.len()
            )
        } else {
            name.to_string()
        };
        let r = run_local_single(
            &label,
            &selected,
            do_apply,
            verbose,
            acknowledged,
            accept_caps,
        )?;
        tot.0 += r.0;
        tot.1 += r.1;
        tot.2 += r.2;
        tot.3 += r.3;
    }
    Ok(tot)
}

pub fn run_local_single(
    name: &str,
    job: &SingleTargetJob,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
) -> std::io::Result<(u64, u64, u64, u64)> {
    let plan = compare_job_detailed(job, &crate::obs::progress::RunCtx::null(), accept_caps)?.plan;
    crate::log_info!(
        "run",
        "[{name}] {} op(s), {} conflict(s)",
        plan.header.op_count,
        plan.header.conflict_count
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
    // The CLI records here; the desktop records at its command boundary to retain its authorization identity.
    let t0 = std::time::Instant::now();
    let rec = crate::run::history::Recorder::start(
        crate::run::history::RunSubject::for_job(name, job),
        super::super::apply_run_kind(job),
        &crate::obs::progress::RunCtx::null(),
        &ops,
    );
    let out = apply_job_guarded_with(
        job,
        &plan,
        &ops,
        None,
        verbose,
        acknowledged,
        accept_caps,
        &rec.ctx,
    );
    let _ = rec.finish(&out, t0.elapsed().as_millis() as u64);
    Ok((
        out.done,
        out.skipped,
        out.errors,
        plan.header.conflict_count,
    ))
}
