//! Exact multi-target job resolution shared by Compare-result lookup and operation execution.

use syncdash::job;

/// Resolve a multi-target job to the engine's single-target view and preserve the normalized
/// index used by result and authorization identities.
pub(crate) fn resolve_target(
    job: &job::Job,
    target_index: Option<usize>,
) -> Result<(usize, job::SingleTargetJob), String> {
    let index = target_index.unwrap_or(0);
    job.select_target(index).map(|target| (index, target))
}
