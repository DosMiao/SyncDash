use syncdash::job;

use crate::features::jobs::target::resolve_target;
use crate::features::operations::authorization::target::JobTargetRevision;

use super::model::ResolvedJobTarget;

pub(in crate::features::operations) fn load_review_target(
    expected_job_id: &str,
    target_index: Option<usize>,
) -> Result<ResolvedJobTarget, String> {
    let (job_name, registered_job) = job::load_by_id(expected_job_id).map_err(|error| {
        format!("The selected job was deleted or replaced — refresh it and review Compare again: {error}")
    })?;
    resolve_job_target(job_name, registered_job, target_index)
}

pub(in crate::features::operations) fn load_bound_target(
    target: &JobTargetRevision,
) -> Result<ResolvedJobTarget, String> {
    let (job_name, registered_job) = job::load_by_id(target.job_id()).map_err(|error| {
        format!("The authorized job was deleted or replaced — review the operation again: {error}")
    })?;
    let resolved = resolve_job_target(job_name, registered_job, Some(target.target_index()))?;
    if resolved.config_revision != target.config_revision() {
        return Err(format!(
            "Job '{}' changed after authorization — review the operation again",
            resolved.job_name
        ));
    }
    Ok(resolved)
}

pub(in crate::features::operations) fn resolve_job_target(
    job_name: String,
    registered_job: syncdash::job::Job,
    target_index: Option<usize>,
) -> Result<ResolvedJobTarget, String> {
    let config_revision = job::config_revision(&registered_job)
        .map_err(|error| format!("Job '{job_name}': {error}"))?;
    let (target_index, target_job) = resolve_target(&registered_job, target_index)?;
    Ok(ResolvedJobTarget {
        job_name,
        registered_job,
        target_index,
        target_job,
        config_revision,
    })
}

fn validate_resolved_target_unchanged(
    reviewed: &ResolvedJobTarget,
    current: &ResolvedJobTarget,
) -> Result<(), String> {
    if reviewed.registered_job.job_id != current.registered_job.job_id {
        return Err("The reviewed job identity changed — review the operation again".into());
    }
    if reviewed.config_revision != current.config_revision {
        return Err("The reviewed job configuration changed — review the operation again".into());
    }
    if reviewed.target_index != current.target_index {
        return Err("The reviewed target changed — review the operation again".into());
    }
    Ok(())
}

pub(in crate::features::operations) fn reload_prepared_target(
    reviewed: &ResolvedJobTarget,
) -> Result<ResolvedJobTarget, String> {
    let (job_name, registered_job) =
        job::load_by_id(&reviewed.registered_job.job_id).map_err(|error| {
            format!("The reviewed job was deleted or replaced — review again: {error}")
        })?;
    let current = resolve_job_target(job_name, registered_job, Some(reviewed.target_index))?;
    validate_resolved_target_unchanged(reviewed, &current)?;
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved_target(name: &str, revision: &str, target_index: usize) -> ResolvedJobTarget {
        let registered_job = syncdash::job::Job {
            job_id: "job-a".into(),
            source: "/source".into(),
            targets: (0..=target_index)
                .map(|index| format!("/target-{index}"))
                .collect(),
            ..Default::default()
        };
        let target_job = registered_job
            .select_target(target_index)
            .expect("fixture target should resolve");
        ResolvedJobTarget {
            job_name: name.into(),
            target_job,
            registered_job,
            target_index,
            config_revision: revision.into(),
        }
    }

    #[test]
    fn registry_identity_is_rechecked_after_slow_probes_before_reservation() {
        let reviewed = resolved_target("photos", "revision-a", 1);
        assert!(validate_resolved_target_unchanged(&reviewed, &reviewed).is_ok());

        let renamed = resolved_target("archive", "revision-a", 1);
        assert!(validate_resolved_target_unchanged(&reviewed, &renamed).is_ok());
        let revised = resolved_target("photos", "revision-b", 1);
        assert!(validate_resolved_target_unchanged(&reviewed, &revised).is_err());
        let retargeted = resolved_target("photos", "revision-a", 0);
        assert!(validate_resolved_target_unchanged(&reviewed, &retargeted).is_err());
        let mut replaced = resolved_target("photos", "revision-a", 1);
        replaced.registered_job.job_id = "job-b".into();
        assert!(validate_resolved_target_unchanged(&reviewed, &replaced).is_err());
    }
}
