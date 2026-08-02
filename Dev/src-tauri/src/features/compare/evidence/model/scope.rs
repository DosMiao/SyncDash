//! Stable job-revision-target identity for Compare execution state.

use crate::contracts::compare::{CompareIdentity, CompareScopeDto};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CompareScope {
    pub(in crate::features::compare::evidence) job_id: String,
    pub(in crate::features::compare::evidence) target_index: usize,
    pub(in crate::features::compare::evidence) config_revision: String,
}

impl CompareScope {
    pub(crate) fn new(job_id: &str, target_index: usize, config_revision: &str) -> Self {
        Self {
            job_id: job_id.to_string(),
            target_index,
            config_revision: config_revision.to_string(),
        }
    }

    pub(in crate::features::compare::evidence) fn from_identity(
        identity: &CompareIdentity,
    ) -> Self {
        Self::new(
            &identity.job_id,
            identity.target_index,
            &identity.config_revision,
        )
    }

    pub(crate) fn contains(&self, identity: &CompareIdentity) -> bool {
        self == &Self::from_identity(identity)
    }

    pub(in crate::features::compare::evidence) fn dto(&self) -> CompareScopeDto {
        CompareScopeDto {
            job_id: self.job_id.clone(),
            target_index: self.target_index,
            config_revision: self.config_revision.clone(),
        }
    }
}
