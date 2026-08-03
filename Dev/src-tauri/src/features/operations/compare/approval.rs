//! One-use approval challenge consumption for interactive Apply.

use std::sync::Arc;

use crate::contracts::operations::{AuthorizationDto, OperationApprovalDto};
use crate::features::operations::authorization::challenge::ReviewApproval;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::lifecycle::RunLifecycle;

use super::super::projection::authorization_dto;

pub(crate) fn approve_operation(
    lifecycle: &Arc<RunLifecycle>,
    authorizations: &Arc<OperationAuthorizationStore>,
    challenge_id: String,
    approval: OperationApprovalDto,
) -> Result<AuthorizationDto, String> {
    let _command = lifecycle.command_lease()?;
    let approval = match approval {
        OperationApprovalDto::InteractiveApply => ReviewApproval::InteractiveApply,
    };
    let issued = authorizations.approve_review_challenge(&challenge_id, approval)?;
    Ok(authorization_dto(issued))
}
