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
    Unavailable,
    Degraded,
    Info,
}

impl From<syncdash::pipeline::guard::caps::CapSeverity> for CapabilitySeverityDto {
    fn from(value: syncdash::pipeline::guard::caps::CapSeverity) -> Self {
        match value {
            syncdash::pipeline::guard::caps::CapSeverity::Unavailable => Self::Unavailable,
            syncdash::pipeline::guard::caps::CapSeverity::Degraded => Self::Degraded,
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
    InteractiveApplyConfirmationRequired {
        challenge_id: String,
        #[ts(type = "number")]
        expires_at_ms: u64,
        warnings: Vec<String>,
        capabilities: Vec<CapabilityIssueDto>,
    },
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "operation", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum OperationApprovalDto {
    InteractiveApply,
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
    pub(crate) cancelled: bool,
}
