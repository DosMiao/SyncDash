//! One-use approval challenge consumption for Compare and interactive Apply.

use std::sync::Arc;

use crate::contracts::operations::{
    ApplySessionGrantDecisionDto, AuthorizationDto, OperationApprovalDto,
};
use crate::features::operations::authorization::challenge::{
    ApplySessionGrantDecision, ReviewApproval,
};
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;

use super::super::projection::authorization_dto;

pub(crate) fn approve_operation(
    lifecycle: &Arc<RunLifecycle>,
    authorizations: &Arc<OperationAuthorizationStore>,
    challenge_id: String,
    approval: OperationApprovalDto,
) -> Result<AuthorizationDto, String> {
    let _command = lifecycle.command_lease()?;
    let approval = match approval {
        OperationApprovalDto::Compare {
            accept_capabilities,
            remember_for_session,
        } => ReviewApproval::Compare {
            accept_capabilities,
            remember_for_session,
        },
        OperationApprovalDto::InteractiveApply {
            acknowledge_health,
            accept_capabilities,
            session_grant,
        } => ReviewApproval::InteractiveApply {
            acknowledge_health,
            accept_capabilities,
            session_grant: match session_grant {
                ApplySessionGrantDecisionDto::None => ApplySessionGrantDecision::None,
                ApplySessionGrantDecisionDto::RememberCapabilities => {
                    ApplySessionGrantDecision::RememberCapabilities
                }
                ApplySessionGrantDecisionDto::AllowAutoApply => {
                    ApplySessionGrantDecision::AllowAutoApply
                }
            },
        },
    };
    let issued = authorizations.approve_review_challenge(&challenge_id, approval)?;
    Ok(authorization_dto(issued))
}
