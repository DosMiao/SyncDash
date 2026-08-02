//! Immutable Compare-result loading and reviewed-operation selection.

use syncdash::job;
use syncdash::model::plan::{Op, Plan};

use crate::contracts::compare::{CompareIdentity, CompareOwner, ReviewedRowDecisionDto};
use crate::features::autoscan::authority::AutoApplyTicket;
use crate::features::compare::evidence::repository::validation::validate_retained_compare;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::decisions::resolve_reviewed_operations;
use crate::features::operations::target::resolve_job_target;

use super::model::{PreparedApply, RetainedApplyPlan};

pub(in crate::features::operations::apply) fn prepare_apply(
    results: &CompareResultRepository,
    compare_identity: &CompareIdentity,
    reviewed_row_decisions: Vec<ReviewedRowDecisionDto>,
) -> Result<PreparedApply, String> {
    prepare_retained_apply(
        load_retained_apply(results, compare_identity)?,
        reviewed_row_decisions,
    )
}

fn load_retained_apply(
    results: &CompareResultRepository,
    compare_identity: &CompareIdentity,
) -> Result<RetainedApplyPlan, String> {
    let (job_name, registered_job) =
        job::load_by_id(&compare_identity.job_id).map_err(|error| {
            format!("The Compare result's job was deleted or replaced — run Compare again: {error}")
        })?;
    let target = resolve_job_target(
        job_name,
        registered_job,
        Some(compare_identity.target_index),
    )?;
    if target.config_revision != compare_identity.config_revision {
        return Err(format!(
            "Job '{}' changed since this Compare — run Compare again",
            target.job_name
        ));
    }
    results
        .rebind_job_name(&target.registered_job.job_id, &target.job_name)
        .map_err(|error| error.to_string())?;
    let retained = results
        .get_fresh_exact(compare_identity)
        .map_err(|error| error.to_string())?;
    let requested_owner = CompareOwner {
        identity: compare_identity.clone(),
        job_name: target.job_name.clone(),
    };
    validate_retained_compare(
        Some(&retained),
        &requested_owner,
        &target.registered_job.job_id,
        &target.job_name,
        target.target_index,
        &target.config_revision,
        None,
    )?;
    let owner = retained.owner().clone();
    let plan = Plan {
        header: retained.plan_header().clone(),
        ops: retained.plan_operations().to_vec(),
    };
    let plan_digest = retained.plan_digest().to_string();
    if plan.digest() != plan_digest {
        return Err("The retained Compare plan changed — run Compare again".into());
    }
    Ok(RetainedApplyPlan {
        target,
        owner,
        plan,
        plan_digest,
    })
}

fn prepare_retained_apply(
    retained_plan: RetainedApplyPlan,
    reviewed_row_decisions: Vec<ReviewedRowDecisionDto>,
) -> Result<PreparedApply, String> {
    let reviewed_operations =
        resolve_reviewed_operations(&retained_plan.plan.ops, &reviewed_row_decisions)?;
    Ok(PreparedApply {
        target: retained_plan.target,
        owner: retained_plan.owner,
        plan: retained_plan.plan,
        plan_digest: retained_plan.plan_digest,
        reviewed_row_decisions,
        reviewed_operations,
    })
}

fn server_owned_reviewed_row_decisions(ops: &[Op]) -> Result<Vec<ReviewedRowDecisionDto>, String> {
    let reviewed_row_decisions: Vec<ReviewedRowDecisionDto> = ops
        .iter()
        .enumerate()
        .filter(|(_, operation)| operation.action.is_executable())
        .map(|(index, _)| ReviewedRowDecisionDto {
            index,
            direction_reversed: false,
        })
        .collect();
    if reviewed_row_decisions.is_empty() {
        return Err(
            "AutoScan found no executable operations; unattended Apply will not run a no-op plan"
                .into(),
        );
    }
    Ok(reviewed_row_decisions)
}

pub(in crate::features::operations::apply) fn prepare_autoscan_apply(
    results: &CompareResultRepository,
    ticket: &AutoApplyTicket,
) -> Result<PreparedApply, String> {
    let retained_plan = load_retained_apply(results, ticket.compare_identity())?;
    if retained_plan.owner.identity != *ticket.compare_identity() {
        return Err("The completed AutoScan ticket no longer owns this Compare result".into());
    }
    let reviewed_row_decisions = server_owned_reviewed_row_decisions(&retained_plan.plan.ops)?;
    prepare_retained_apply(retained_plan, reviewed_row_decisions)
}

#[cfg(test)]
mod tests {
    use syncdash::model::plan::{Action, Side};

    use super::*;

    fn op(action: Action, path: &str) -> Op {
        Op {
            side: Side::Target,
            action,
            path: path.into(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "test".into(),
        }
    }

    #[test]
    fn autoscan_review_decisions_are_server_owned_complete_ordered_and_not_reversed() {
        let ops = vec![
            op(Action::Copy, "copy"),
            op(Action::Conflict, "conflict"),
            op(Action::Note, "note"),
            op(Action::Delete, "delete"),
        ];
        assert_eq!(
            server_owned_reviewed_row_decisions(&ops).unwrap(),
            vec![
                ReviewedRowDecisionDto {
                    index: 0,
                    direction_reversed: false
                },
                ReviewedRowDecisionDto {
                    index: 3,
                    direction_reversed: false
                },
            ]
        );
        assert!(server_owned_reviewed_row_decisions(&ops[1..3]).is_err());
    }
}
