//! Compare capability review and exact authorization issuance.

use std::sync::Arc;

use syncdash::pipeline::guard::caps::CapReport;
use syncdash::run;

use crate::contracts::compare::AutoScanCompareRequestDto;
use crate::contracts::operations::OperationReviewDto;
use crate::features::autoscan::controller::AutoScanController;
use crate::features::autoscan::model::AutoScanVerificationTerminal;
use crate::features::operations::authorization::compare::CompareOrigin;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::lifecycle::RunLifecycle;

use super::super::execution::error::{format_run_io_error, verification_terminal_from_io};
use super::super::projection::{authorization_dto, blocked_review, capability_dtos};
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
            let origin = match auto_scan_request {
                Some(request) => CompareOrigin::AutoScan(
                    autoscan.issue_compare_permit(request.generation, request.ticket_id)?,
                ),
                None => CompareOrigin::Interactive,
            };
            // Compare only ever reads. Its capability list rides along with the authorization so
            // the workspace can show what this comparison will and will not be able to see.
            let authorization = build_compare_authorization(&target, origin)?;
            let issued = authorizations.issue_compare_authorization(authorization)?;
            Ok(OperationReviewDto::DirectAuthorized {
                authorization: authorization_dto(issued),
                capabilities: capability_dtos(&capabilities),
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
                Ok(OperationReviewDto::InteractiveApplyConfirmationRequired { .. }) => {
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
