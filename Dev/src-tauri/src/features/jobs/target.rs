//! Exact target selection for multi-target jobs.

use syncdash::job;

pub(crate) fn resolve_target(
    job: &job::Job,
    target_index: Option<usize>,
) -> Result<(usize, job::SingleTargetJob), String> {
    let index = target_index.unwrap_or(0);
    job.select_target(index).map(|target| (index, target))
}
