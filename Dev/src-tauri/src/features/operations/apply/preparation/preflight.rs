//! Apply health/capability preflight and stable review fingerprint construction.

use syncdash::pipeline::guard::caps::CapabilityScope;
use syncdash::run;

use crate::features::operations::authorization::apply::ApplyReview;
use crate::features::operations::authorization::digest::health_review_digest;
use crate::features::operations::execution::error::format_run_io_error;
use crate::features::operations::projection::capability_blockers;

use super::model::{ApplyFacts, PreparedApply};

pub(in crate::features::operations::apply) fn apply_facts(
    prepared: &PreparedApply,
) -> Result<ApplyFacts, String> {
    let requirements = run::apply_requirements(
        &prepared.target.target_job,
        &prepared.plan,
        &prepared.reviewed_operations,
        false,
    )
    .map_err(format_run_io_error)?;
    let acknowledged = if requirements.verdict.ok() {
        requirements.verdict.clone()
    } else {
        run::preflight(
            &prepared.target.target_job,
            &prepared.plan,
            &prepared.reviewed_operations,
            true,
        )
        .map_err(format_run_io_error)?
    };
    Ok(ApplyFacts {
        unacknowledged: requirements.verdict,
        acknowledged,
        capabilities: requirements.capabilities,
    })
}

pub(in crate::features::operations::apply) fn build_apply_review(
    prepared: &PreparedApply,
    facts: &ApplyFacts,
) -> Result<ApplyReview, String> {
    ApplyReview::new(
        prepared.owner.identity.clone(),
        prepared.plan_digest.clone(),
        prepared.reviewed_row_decisions.clone(),
        health_review_digest(&facts.unacknowledged, &facts.acknowledged),
        facts
            .capabilities
            .consent_digest(CapabilityScope::ApplyWrite),
    )
}

pub(in crate::features::operations::apply) fn apply_review_messages(
    facts: &ApplyFacts,
) -> (Vec<String>, Vec<String>, bool) {
    let requires_health_ack = !facts.unacknowledged.ok() && facts.acknowledged.ok();
    let mut blockers = facts.acknowledged.blockers.clone();
    blockers.extend(capability_blockers(&facts.capabilities));
    let mut warnings = facts.unacknowledged.warnings.clone();
    if requires_health_ack {
        warnings.extend(facts.unacknowledged.blockers.clone());
    } else {
        warnings.extend(facts.acknowledged.warnings.clone());
    }
    warnings.sort();
    warnings.dedup();
    (blockers, warnings, requires_health_ack)
}

pub(in crate::features::operations::apply) fn autoscan_health_refusals(
    facts: &ApplyFacts,
) -> Vec<String> {
    let mut messages = facts.unacknowledged.blockers.clone();
    messages.extend(facts.unacknowledged.warnings.clone());
    messages.sort();
    messages.dedup();
    messages
}

#[cfg(test)]
mod tests {
    use syncdash::pipeline::guard::caps::CapReport;
    use syncdash::pipeline::guard::Verdict;

    use super::*;

    #[test]
    fn autoscan_health_refuses_warning_only_verdicts_deterministically() {
        let facts = ApplyFacts {
            unacknowledged: Verdict {
                blockers: Vec::new(),
                warnings: vec!["z warning".into(), "a warning".into(), "a warning".into()],
            },
            acknowledged: Verdict {
                blockers: Vec::new(),
                warnings: Vec::new(),
            },
            capabilities: CapReport::default(),
        };
        assert_eq!(
            autoscan_health_refusals(&facts),
            vec!["a warning".to_string(), "z warning".to_string()]
        );
    }
}
