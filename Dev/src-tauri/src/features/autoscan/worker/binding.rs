//! Resolution of a generation's immutable job binding against the live registry.

use super::super::model::AutoScanBinding;

pub(super) fn resolve_binding_job_name(binding: &AutoScanBinding) -> Result<String, String> {
    let (job_name, job) = syncdash::job::load_by_id(&binding.job_id).map_err(|error| {
        format!(
            "job '{}' no longer has registry identity '{}': {error}",
            binding.job_name, binding.job_id
        )
    })?;
    validate_resolved_binding(binding, &job_name, &job)?;
    Ok(job_name)
}

pub(in crate::features::autoscan) fn validate_resolved_binding(
    binding: &AutoScanBinding,
    job_name: &str,
    job: &syncdash::job::Job,
) -> Result<(), String> {
    if job.job_id != binding.job_id {
        return Err(format!(
            "job name '{job_name}' now belongs to a replacement identity"
        ));
    }
    let revision = syncdash::job::config_revision(job)
        .map_err(|error| format!("job '{job_name}' cannot be fingerprinted: {error}"))?;
    if revision != binding.config_revision {
        return Err(format!(
            "job '{job_name}' changed configuration after this AutoScan generation started"
        ));
    }
    if binding.target_index >= job.targets.len() {
        return Err(format!(
            "job '{job_name}' no longer has target {}",
            binding.target_index + 1
        ));
    }
    Ok(())
}
