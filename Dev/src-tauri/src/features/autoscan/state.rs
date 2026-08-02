//! One-generation ticket state machine and its observable status projection.

use crate::features::compare::evidence::model::verification::CompareVerificationTicket;
use crate::features::operations::autoscan_authority::{AutoApplyTicket, AutoScanComparePermit};

use super::model::{AutoScanBinding, AutoScanStatusDto, AutoScanStatusMode, AutoScanTriggerDto};

#[derive(Clone, Debug)]
pub(super) struct AutoScanStatusCore {
    pub(super) active: bool,
    pub(super) generation: u64,
    pub(super) job_id: Option<String>,
    pub(super) job_name: Option<String>,
    pub(super) config_revision: Option<String>,
    pub(super) target_index: Option<usize>,
    pub(super) interval_secs: Option<u64>,
    pub(super) auto_apply: bool,
    pub(super) mode: Option<AutoScanStatusMode>,
    pub(super) detail: String,
    pub(super) latest_ticket_id: u64,
}

impl AutoScanStatusCore {
    pub(super) fn starting(generation: u64, binding: &AutoScanBinding) -> Self {
        Self {
            active: true,
            generation,
            job_id: Some(binding.job_id.clone()),
            job_name: Some(binding.job_name.clone()),
            config_revision: Some(binding.config_revision.clone()),
            target_index: Some(binding.target_index),
            interval_secs: Some(binding.interval_secs),
            auto_apply: binding.auto_apply,
            mode: Some(AutoScanStatusMode::Starting),
            detail: "Preparing backend-owned change detection".into(),
            latest_ticket_id: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum AutoScanTicketLifecycle {
    Idle,
    AwaitingCompare {
        trigger: AutoScanTriggerDto,
        verification: CompareVerificationTicket,
    },
    ComparePermitted {
        trigger: AutoScanTriggerDto,
        permit: AutoScanComparePermit,
    },
    CompareRunning {
        trigger: AutoScanTriggerDto,
        permit: AutoScanComparePermit,
    },
    AutoApplyCompleted {
        ticket: AutoApplyTicket,
    },
    AutoApplyClaimed {
        ticket: AutoApplyTicket,
    },
    AutoApplyAuthorized {
        ticket: AutoApplyTicket,
    },
}

impl AutoScanTicketLifecycle {
    pub(super) fn pending_trigger(&self) -> Option<&AutoScanTriggerDto> {
        match self {
            Self::AwaitingCompare { trigger, .. }
            | Self::ComparePermitted { trigger, .. }
            | Self::CompareRunning { trigger, .. } => Some(trigger),
            Self::Idle
            | Self::AutoApplyCompleted { .. }
            | Self::AutoApplyClaimed { .. }
            | Self::AutoApplyAuthorized { .. } => None,
        }
    }

    pub(super) fn rebind_job_name(&mut self, job_name: &str) {
        match self {
            Self::AwaitingCompare { trigger, .. }
            | Self::ComparePermitted { trigger, .. }
            | Self::CompareRunning { trigger, .. } => {
                trigger.job_name = job_name.to_string();
            }
            Self::Idle
            | Self::AutoApplyCompleted { .. }
            | Self::AutoApplyClaimed { .. }
            | Self::AutoApplyAuthorized { .. } => {}
        }
    }

    pub(super) fn verification(&self) -> Option<&CompareVerificationTicket> {
        match self {
            Self::AwaitingCompare { verification, .. } => Some(verification),
            Self::ComparePermitted { permit, .. } | Self::CompareRunning { permit, .. } => {
                Some(permit.verification())
            }
            Self::Idle
            | Self::AutoApplyCompleted { .. }
            | Self::AutoApplyClaimed { .. }
            | Self::AutoApplyAuthorized { .. } => None,
        }
    }

    pub(super) fn owns_permit(&self, expected: &AutoScanComparePermit) -> bool {
        matches!(
            self,
            Self::ComparePermitted { permit, .. } | Self::CompareRunning { permit, .. }
                if permit == expected
        )
    }

    pub(super) fn can_be_declined(&self) -> bool {
        matches!(
            self,
            Self::AwaitingCompare { .. } | Self::ComparePermitted { .. }
        )
    }
}

pub(super) struct AutoScanShared {
    pub(super) status: AutoScanStatusCore,
    pub(super) ticket: AutoScanTicketLifecycle,
}

impl AutoScanShared {
    pub(super) fn snapshot(&self) -> AutoScanStatusDto {
        let pending_trigger = self.ticket.pending_trigger().cloned();
        AutoScanStatusDto {
            active: self.status.active,
            generation: self.status.generation,
            job_id: self.status.job_id.clone(),
            job_name: self.status.job_name.clone(),
            config_revision: self.status.config_revision.clone(),
            target_index: self.status.target_index,
            interval_secs: self.status.interval_secs,
            auto_apply: self.status.auto_apply,
            mode: self.status.mode,
            detail: self.status.detail.clone(),
            latest_ticket_id: self.status.latest_ticket_id,
            active_ticket: pending_trigger.as_ref().map(|trigger| trigger.ticket_id),
            pending_trigger,
        }
    }
}
