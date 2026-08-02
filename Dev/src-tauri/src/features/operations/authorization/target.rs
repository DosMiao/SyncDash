//! Stable job/revision/target identity shared by every authorization path.

use crate::contracts::compare::CompareIdentity;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JobTargetRevision {
    job_id: String,
    config_revision: String,
    target_index: usize,
}

impl JobTargetRevision {
    pub(crate) fn new(
        job_id: String,
        config_revision: String,
        target_index: usize,
    ) -> Result<Self, String> {
        if job_id.is_empty() || config_revision.is_empty() {
            return Err("The operation target identity is incomplete".into());
        }
        Ok(Self {
            job_id,
            config_revision,
            target_index,
        })
    }

    pub(crate) fn job_id(&self) -> &str {
        &self.job_id
    }

    pub(crate) fn config_revision(&self) -> &str {
        &self.config_revision
    }

    pub(crate) fn target_index(&self) -> usize {
        self.target_index
    }
}

impl From<&CompareIdentity> for JobTargetRevision {
    fn from(identity: &CompareIdentity) -> Self {
        Self {
            job_id: identity.job_id.clone(),
            config_revision: identity.config_revision.clone(),
            target_index: identity.target_index,
        }
    }
}
