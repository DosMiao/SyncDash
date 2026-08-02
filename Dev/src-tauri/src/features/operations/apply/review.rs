//! Interactive Apply review and unattended AutoScan authorization commands.

use std::sync::Arc;

use syncdash::pipeline::guard::caps::CapReport;

use crate::contracts::compare::{CompareIdentity, ReviewedRowDecisionDto};
use crate::contracts::operations::{AuthorizationDto, OperationReviewDto};
use crate::features::autoscan::controller::AutoScanController;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::challenge::ReviewChallenge;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;

use super::super::projection::{
    authorization_dto, blocked_review, capability_blockers, capability_dtos,
};
use super::super::target::reload_prepared_target;
use super::preparation::{
    apply_facts, apply_review_messages, autoscan_health_refusals, build_apply_review,
    prepare_apply, prepare_autoscan_apply,
};

pub(crate) async fn review_apply(
    lifecycle: Arc<RunLifecycle>,
    results: Arc<CompareResultRepository>,
    authorizations: Arc<OperationAuthorizationStore>,
    compare_identity: CompareIdentity,
    reviewed_row_decisions: Vec<ReviewedRowDecisionDto>,
) -> Result<OperationReviewDto, String> {
    let _command = lifecycle.command_lease()?;
    tauri::async_runtime::spawn_blocking(move || {
        let prepared = prepare_apply(&results, &compare_identity, reviewed_row_decisions)?;
        let facts = match apply_facts(&prepared) {
            Ok(facts) => facts,
            Err(error) => {
                return Ok(blocked_review(
                    vec![error],
                    Vec::new(),
                    &CapReport::default(),
                ))
            }
        };
        let (blockers, warnings, requires_health_ack) = apply_review_messages(&facts);
        if !blockers.is_empty() {
            return Ok(blocked_review(blockers, warnings, &facts.capabilities));
        }
        let review = build_apply_review(&prepared, &facts)?;
        let requires_capability_ack = !facts.capabilities.needs_ack().is_empty()
            && !authorizations.has_interactive_apply_capability_grant(&review);
        let compare_identity = review.compare_identity().clone();
        let challenge = results.with_fresh_execution_eligibility(&compare_identity, || {
            authorizations.create_review_challenge(ReviewChallenge::InteractiveApply {
                review,
                requires_health_ack,
                requires_capability_ack,
            })
        })?;
        Ok(OperationReviewDto::InteractiveApplyConfirmationRequired {
            challenge_id: challenge.challenge_id,
            expires_at_ms: challenge.expires_at_ms,
            warnings,
            capabilities: capability_dtos(&facts.capabilities),
            requires_health_ack,
            requires_capability_ack,
            can_remember_for_session: true,
            can_allow_unattended: true,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

pub(crate) async fn authorize_autoscan_apply(
    lifecycle: Arc<RunLifecycle>,
    results: Arc<CompareResultRepository>,
    authorizations: Arc<OperationAuthorizationStore>,
    autoscan: Arc<AutoScanController>,
    generation: u64,
    ticket_id: u64,
) -> Result<AuthorizationDto, String> {
    let _command = lifecycle.command_lease()?;
    tauri::async_runtime::spawn_blocking(move || {
        let ticket = autoscan.claim_completed_auto_apply(generation, ticket_id)?;
        let prepared = prepare_autoscan_apply(&results, &ticket)?;
        let facts = apply_facts(&prepared)?;
        let health_refusals = autoscan_health_refusals(&facts);
        if !health_refusals.is_empty() {
            return Err(format!(
                "AutoScan Apply requires a completely clean health review:\n{}",
                health_refusals.join("\n")
            ));
        }
        let blockers = capability_blockers(&facts.capabilities);
        if !blockers.is_empty() {
            return Err(blockers.join("\n"));
        }
        reload_prepared_target(&prepared.target)?;
        let review = build_apply_review(&prepared, &facts)?;
        let compare_identity = review.compare_identity().clone();
        let issued = autoscan.authorize_claim(&ticket, || {
            results.with_fresh_execution_eligibility(&compare_identity, || {
                authorizations.issue_auto_apply_authorization(review, ticket.clone())
            })
        })?;
        Ok(authorization_dto(issued))
    })
    .await
    .map_err(|error| error.to_string())?
}
