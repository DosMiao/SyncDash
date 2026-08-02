//! Successful Compare evidence projection and post-run job-revision validation.

use syncdash::model::plan::Action;
use syncdash::pipeline::compare as pipeline_compare;
use syncdash::{job, run::CompareOutcome};

use crate::contracts::compare::{CompareIdentity, CompareOwner, PlanDto};
use crate::features::compare::evidence::model::result::SuccessfulCompareResult;

use crate::features::operations::target::ResolvedJobTarget;

pub(super) fn prepare_successful_result(
    run_id: u64,
    target: &ResolvedJobTarget,
    outcome: CompareOutcome,
) -> Result<SuccessfulCompareResult, String> {
    let plan_digest = outcome.plan.digest();
    let result_id = crate::secure_random::random_hex::<16>(
        "Cannot allocate the successful Compare result identity",
    )?;
    let mut owner = CompareOwner {
        identity: CompareIdentity {
            result_id,
            compare_run_id: run_id,
            job_id: target.registered_job.job_id.clone(),
            target_index: target.target_index,
            config_revision: target.config_revision.clone(),
        },
        job_name: target.job_name.clone(),
    };
    let evidence = pipeline_compare::evidence::evidence(
        &outcome.source,
        &outcome.target,
        &outcome.plan,
        &outcome.compare_options,
    );
    let metas = evidence
        .metas
        .into_iter()
        .zip(&outcome.plan.ops)
        .map(|(meta, operation)| {
            if matches!(operation.action, Action::Copy)
                && operation.size.is_some()
                && operation.mtime_ms.is_some()
            {
                None
            } else {
                Some(meta)
            }
        })
        .collect();
    let mut plan = PlanDto {
        owner: owner.clone(),
        header: outcome.plan.header,
        ops: outcome.plan.ops,
        metas,
        identical_count: evidence.identical_count,
        identical_bytes: evidence.identical_bytes,
    };

    let (current_name, current_job) = job::load_by_id(&owner.identity.job_id).map_err(|error| {
        format!(
            "Job '{}' was deleted or replaced while Compare was running: {error}",
            owner.job_name
        )
    })?;
    let current_revision = job::config_revision(&current_job)
        .map_err(|error| format!("Job '{}': {error}", owner.job_name))?;
    if current_revision != owner.identity.config_revision {
        return Err(format!(
            "Job '{}' changed while Compare was running — run Compare again",
            owner.job_name
        ));
    }
    owner.job_name = current_name;
    plan.owner = owner;
    Ok(SuccessfulCompareResult::from_plan(
        plan_digest,
        plan,
        outcome.source,
        outcome.target,
        outcome.compare_options,
    ))
}
