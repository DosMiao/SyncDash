use syncdash::pipeline::guard::caps::CapReport;

use crate::contracts::operations::{AuthorizationDto, CapabilityIssueDto, OperationReviewDto};
use crate::features::operations::authorization::challenge::IssuedAuthorization;

pub(super) fn authorization_dto(issued: IssuedAuthorization) -> AuthorizationDto {
    AuthorizationDto {
        authorization_token: issued.authorization_token,
        expires_at_ms: issued.expires_at_ms,
    }
}

pub(super) fn capability_dtos(capabilities: &CapReport) -> Vec<CapabilityIssueDto> {
    capabilities.items.iter().map(Into::into).collect()
}

pub(super) fn capability_blockers(capabilities: &CapReport) -> Vec<String> {
    capabilities
        .blockers()
        .into_iter()
        .map(|item| item.render())
        .collect()
}

pub(super) fn blocked_review(
    blockers: Vec<String>,
    warnings: Vec<String>,
    capabilities: &CapReport,
) -> OperationReviewDto {
    OperationReviewDto::Blocked {
        blockers,
        warnings,
        capabilities: capability_dtos(capabilities),
    }
}
