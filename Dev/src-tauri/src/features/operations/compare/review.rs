//! Compare capability review and exact authorization issuance.

use std::sync::Arc;

use syncdash::pipeline::guard::caps::CapReport;
use syncdash::run;

use crate::contracts::compare::AutoScanCompareRequestDto;
use crate::contracts::operations::OperationReviewDto;
use crate::features::autoscan::controller::AutoScanController;
use crate::features::autoscan::model::AutoScanVerificationTerminal;
use crate::features::operations::authorization::challenge::ReviewChallenge;
use crate::features::operations::authorization::compare::CompareOrigin;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;

use super::super::execution::error::{format_run_io_error, verification_terminal_from_io};
use super::super::projection::{
    authorization_dto, blocked_review, capability_blockers, capability_dtos,
};
use super::super::target::{build_compare_authorization, load_review_target};

pub(crate) async fn review_compare(
    lifecycle: Arc<RunLifecycle>,
    authorizations: Arc<OperationAuthorizationStore>,
    autoscan: Arc<AutoScanController>,
    expected_job_id: String,
    target_index: Option<usize>,
    auto_scan_request: Option<AutoScanCompareRequestDto>,
) -> Result<OperationReviewDto, String> {
    let _command = match lifecycle.command_lease() {
        Ok(command) => command,
        Err(error) => {
            if let Some(request) = auto_scan_request {
                let _ = autoscan.terminalize_verification_request(
                    request.generation,
                    request.ticket_id,
                    AutoScanVerificationTerminal::Failed(format!(
                        "AutoScan Compare could not enter review: {error}"
                    )),
                );
            }
            return Err(error);
        }
    };
    let join_request = auto_scan_request;
    let join_autoscan = autoscan.clone();
    let task = tauri::async_runtime::spawn_blocking(move || {
        let result = (|| {
            let target = load_review_target(&expected_job_id, target_index)?;
            let capabilities = match run::compare_capabilities(&target.target_job) {
                Ok(capabilities) => capabilities,
                Err(error) => {
                    if let Some(request) = auto_scan_request {
                        let terminal = verification_terminal_from_io(&error);
                        let _ = autoscan.terminalize_verification_request(
                            request.generation,
                            request.ticket_id,
                            terminal,
                        );
                    }
                    return Ok(blocked_review(
                        vec![format_run_io_error(error)],
                        Vec::new(),
                        &CapReport::default(),
                    ));
                }
            };
            let blockers = capability_blockers(&capabilities);
            if !blockers.is_empty() {
                return Ok(blocked_review(blockers, Vec::new(), &capabilities));
            }
            let origin = match auto_scan_request {
                Some(request) => CompareOrigin::AutoScan(
                    autoscan.issue_compare_permit(request.generation, request.ticket_id)?,
                ),
                None => CompareOrigin::Interactive,
            };
            let authorization = build_compare_authorization(&target, &capabilities, origin)?;
            let requires_capability_ack = !capabilities.needs_ack().is_empty();
            if !requires_capability_ack
                || authorizations.has_compare_capability_grant(&authorization)
            {
                let issued = authorizations.issue_compare_authorization(authorization)?;
                return Ok(OperationReviewDto::DirectAuthorized {
                    authorization: authorization_dto(issued),
                    capabilities: capability_dtos(&capabilities),
                });
            }
            if auto_scan_request.is_some() {
                return Ok(blocked_review(
                    vec![
                        "AutoScan Compare requires an interactive capability approval for this exact job revision and target".into(),
                    ],
                    Vec::new(),
                    &capabilities,
                ));
            }
            let challenge = authorizations.create_review_challenge(ReviewChallenge::Compare {
                authorization,
                requires_capability_ack: true,
            })?;
            Ok(OperationReviewDto::CompareConfirmationRequired {
                challenge_id: challenge.challenge_id,
                expires_at_ms: challenge.expires_at_ms,
                capabilities: capability_dtos(&capabilities),
                can_remember_for_session: true,
            })
        })();
        if let Some(request) = auto_scan_request {
            let terminal = match &result {
                Err(error) => Some(AutoScanVerificationTerminal::Failed(format!(
                    "AutoScan Compare review failed: {error}"
                ))),
                Ok(OperationReviewDto::Blocked { blockers, .. }) => {
                    Some(AutoScanVerificationTerminal::Failed(format!(
                        "AutoScan Compare review was blocked: {}",
                        blockers.join("; ")
                    )))
                }
                Ok(OperationReviewDto::CompareConfirmationRequired { .. })
                | Ok(OperationReviewDto::InteractiveApplyConfirmationRequired { .. }) => {
                    Some(AutoScanVerificationTerminal::Failed(
                        "AutoScan Compare unexpectedly required an interactive approval".into(),
                    ))
                }
                Ok(OperationReviewDto::DirectAuthorized { .. }) => None,
            };
            if let Some(terminal) = terminal {
                let _ = autoscan.terminalize_verification_request(
                    request.generation,
                    request.ticket_id,
                    terminal,
                );
            }
        }
        result
    });
    match task.await {
        Ok(result) => result,
        Err(error) => {
            let message = error.to_string();
            if let Some(request) = join_request {
                let _ = join_autoscan.terminalize_verification_request(
                    request.generation,
                    request.ticket_id,
                    AutoScanVerificationTerminal::Failed(format!(
                        "AutoScan Compare review task failed: {message}"
                    )),
                );
            }
            Err(message)
        }
    }
}
