//! Exact one-use authority handed between AutoScan and operation authorization.

use crate::contracts::compare::{CompareIdentity, CompareOwner};
use crate::features::compare::evidence::model::verification::CompareVerificationTicket;

/// Associates one Compare launch with one exact AutoScan trigger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutoScanComparePermit {
    permit_id: u64,
    generation: u64,
    ticket_id: u64,
    job_id: String,
    config_revision: String,
    target_index: usize,
    verification: CompareVerificationTicket,
}

impl AutoScanComparePermit {
    pub(crate) fn new(
        permit_id: u64,
        generation: u64,
        ticket_id: u64,
        job_id: String,
        config_revision: String,
        target_index: usize,
        verification: CompareVerificationTicket,
    ) -> Self {
        Self {
            permit_id,
            generation,
            ticket_id,
            job_id,
            config_revision,
            target_index,
            verification,
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn ticket_id(&self) -> u64 {
        self.ticket_id
    }

    pub(crate) fn owns_compare(&self, owner: &CompareOwner) -> bool {
        owner.identity.job_id == self.job_id
            && owner.identity.config_revision == self.config_revision
            && owner.identity.target_index == self.target_index
    }

    pub(crate) fn verification(&self) -> &CompareVerificationTicket {
        &self.verification
    }
}

/// Binds one completed AutoScan trigger to the Compare result eligible for unattended Apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutoApplyTicket {
    generation: u64,
    ticket_id: u64,
    compare_identity: CompareIdentity,
}

impl AutoApplyTicket {
    pub(crate) fn new(generation: u64, ticket_id: u64, compare_identity: CompareIdentity) -> Self {
        Self {
            generation,
            ticket_id,
            compare_identity,
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn matches_key(&self, generation: u64, ticket_id: u64) -> bool {
        self.generation == generation && self.ticket_id == ticket_id
    }

    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        self == other
    }

    pub(crate) fn compare_identity(&self) -> &CompareIdentity {
        &self.compare_identity
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        generation: u64,
        ticket_id: u64,
        compare_identity: CompareIdentity,
    ) -> Self {
        Self::new(generation, ticket_id, compare_identity)
    }
}
