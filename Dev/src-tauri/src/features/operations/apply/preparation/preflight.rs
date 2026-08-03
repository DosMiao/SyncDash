//! Apply health/capability preflight and stable review fingerprint construction.

use syncdash::run;

use crate::features::operations::authorization::apply::ApplyReview;
use crate::features::operations::authorization::digest::health_review_digest;
use crate::features::operations::execution::error::format_run_io_error;

use super::model::{ApplyFacts, PreparedApply};

pub(in crate::features::operations::apply) fn apply_facts(
    prepared: &PreparedApply,
) -> Result<ApplyFacts, String> {
    let requirements = run::apply_requirements(
        &prepared.target.target_job,
        &prepared.plan,
        &prepared.reviewed_operations,
    )
    .map_err(format_run_io_error)?;
    Ok(ApplyFacts {
        verdict: requirements.verdict,
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
        health_review_digest(&facts.verdict),
    )
}

/// Blockers are the preconditions a run cannot proceed without — a root that is not there, or a
/// disk that cannot hold the writes. Everything the operator should weigh themselves is a warning
/// the review panel shows.
pub(in crate::features::operations::apply) fn apply_review_messages(
    facts: &ApplyFacts,
) -> (Vec<String>, Vec<String>) {
    let mut warnings = facts.verdict.warnings.clone();
    warnings.sort();
    warnings.dedup();
    (facts.verdict.blockers.clone(), warnings)
}

/// An unattended Apply has no operator to weigh a warning, so AutoScan refuses on anything the
/// interactive review would have shown — blockers and warnings alike. Authorization and execution
/// both check this, so the predicate and its refusal have one home.
pub(in crate::features::operations::apply) fn require_clean_autoscan_health(
    facts: &ApplyFacts,
) -> Result<(), String> {
    let health_refusals = autoscan_health_refusals(facts);
    if health_refusals.is_empty() {
        return Ok(());
    }
    Err(format!(
        "AutoScan Apply requires a completely clean health review:\n{}",
        health_refusals.join("\n")
    ))
}

fn autoscan_health_refusals(facts: &ApplyFacts) -> Vec<String> {
    let mut messages = facts.verdict.blockers.clone();
    messages.extend(facts.verdict.warnings.clone());
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
            verdict: Verdict {
                blockers: Vec::new(),
                warnings: vec!["z warning".into(), "a warning".into(), "a warning".into()],
            },
            capabilities: CapReport::default(),
        };
        assert_eq!(
            autoscan_health_refusals(&facts),
            vec!["a warning".to_string(), "z warning".to_string()]
        );
    }
}
