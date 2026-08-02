//! Compare/Apply review and run-result wire contracts.

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum PostRunPowerActionDto {
    Sleep,
    Shutdown,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct PostRunPowerActionReadyDto {
    #[ts(type = "number")]
    pub(crate) run_id: u64,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum CapabilitySeverityDto {
    Block,
    NeedsAck,
    Info,
}

impl From<syncdash::pipeline::guard::caps::CapSeverity> for CapabilitySeverityDto {
    fn from(value: syncdash::pipeline::guard::caps::CapSeverity) -> Self {
        match value {
            syncdash::pipeline::guard::caps::CapSeverity::Block => Self::Block,
            syncdash::pipeline::guard::caps::CapSeverity::NeedsAck => Self::NeedsAck,
            syncdash::pipeline::guard::caps::CapSeverity::Info => Self::Info,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct CapabilityIssueDto {
    pub(crate) feature: String,
    pub(crate) side: String,
    pub(crate) severity: CapabilitySeverityDto,
    pub(crate) requested: String,
    pub(crate) actual: String,
    pub(crate) effect: String,
}

impl From<&syncdash::pipeline::guard::caps::CapItem> for CapabilityIssueDto {
    fn from(value: &syncdash::pipeline::guard::caps::CapItem) -> Self {
        Self {
            feature: value.feature.clone(),
            side: value.side.clone(),
            severity: value.severity.into(),
            requested: value.requested.clone(),
            actual: value.actual.clone(),
            effect: value.effect.clone(),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct AuthorizationDto {
    pub(crate) authorization_token: String,
    #[ts(type = "number")]
    pub(crate) expires_at_ms: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum OperationReviewDto {
    Blocked {
        blockers: Vec<String>,
        warnings: Vec<String>,
        capabilities: Vec<CapabilityIssueDto>,
    },
    DirectAuthorized {
        authorization: AuthorizationDto,
        capabilities: Vec<CapabilityIssueDto>,
    },
    CompareConfirmationRequired {
        challenge_id: String,
        #[ts(type = "number")]
        expires_at_ms: u64,
        capabilities: Vec<CapabilityIssueDto>,
        can_remember_for_session: bool,
    },
    InteractiveApplyConfirmationRequired {
        challenge_id: String,
        #[ts(type = "number")]
        expires_at_ms: u64,
        warnings: Vec<String>,
        capabilities: Vec<CapabilityIssueDto>,
        requires_health_ack: bool,
        requires_capability_ack: bool,
        can_remember_for_session: bool,
        can_allow_unattended: bool,
    },
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "operation", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum OperationApprovalDto {
    Compare {
        accept_capabilities: bool,
        remember_for_session: bool,
    },
    InteractiveApply {
        acknowledge_health: bool,
        accept_capabilities: bool,
        session_grant: ApplySessionGrantDecisionDto,
    },
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum ApplySessionGrantDecisionDto {
    None,
    RememberCapabilities,
    AllowAutoApply,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct ApplyDto {
    #[ts(type = "number")]
    pub(crate) done: u64,
    #[ts(type = "number")]
    pub(crate) skipped: u64,
    #[ts(type = "number")]
    pub(crate) errors: u64,
    #[ts(type = "number")]
    pub(crate) bytes_copied: u64,
    pub(crate) cancelled: bool,
}
