//! Process-local execution freshness and terminal state.

use crate::contracts::compare::{
    CompareExecutionExpiryReasonDto, CompareIdentity, CompareVerificationAttemptDto,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::features::compare::evidence) enum CompareExecutionState {
    AwaitingCompare {
        verification_epoch: u64,
    },
    Comparing {
        verification_epoch: u64,
        launched_run_id: u64,
    },
    Fresh {
        verification_epoch: u64,
        identity: CompareIdentity,
    },
    Failed {
        verification_epoch: u64,
        launched_run_id: Option<u64>,
        message: String,
    },
    Cancelled {
        verification_epoch: u64,
        launched_run_id: Option<u64>,
    },
    Expired {
        verification_epoch: u64,
        launched_run_id: Option<u64>,
        reason: CompareExecutionExpiryReasonDto,
    },
}

impl CompareExecutionState {
    pub(in crate::features::compare::evidence) fn verification_epoch(&self) -> u64 {
        match self {
            Self::AwaitingCompare { verification_epoch }
            | Self::Comparing {
                verification_epoch, ..
            }
            | Self::Fresh {
                verification_epoch, ..
            }
            | Self::Failed {
                verification_epoch, ..
            }
            | Self::Cancelled {
                verification_epoch, ..
            }
            | Self::Expired {
                verification_epoch, ..
            } => *verification_epoch,
        }
    }

    fn launched_run_id(&self) -> Option<u64> {
        match self {
            Self::AwaitingCompare { .. } => None,
            Self::Comparing {
                launched_run_id, ..
            } => Some(*launched_run_id),
            Self::Fresh { identity, .. } => Some(identity.compare_run_id),
            Self::Failed {
                launched_run_id, ..
            }
            | Self::Cancelled {
                launched_run_id, ..
            }
            | Self::Expired {
                launched_run_id, ..
            } => *launched_run_id,
        }
    }

    pub(in crate::features::compare::evidence) fn verification_attempt(
        &self,
    ) -> CompareVerificationAttemptDto {
        CompareVerificationAttemptDto {
            verification_epoch: self.verification_epoch(),
            compare_run_id: self.launched_run_id(),
        }
    }

    pub(in crate::features::compare::evidence) fn expire(
        &mut self,
        reason: CompareExecutionExpiryReasonDto,
    ) {
        let verification_epoch = self.verification_epoch();
        let launched_run_id = self.launched_run_id();
        *self = Self::Expired {
            verification_epoch,
            launched_run_id,
            reason,
        };
    }
}
