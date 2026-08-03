//! Exact Compare authorization binding for a resolved job target.

use crate::features::operations::authorization::compare::{CompareAuthorization, CompareOrigin};
use crate::features::operations::authorization::target::JobTargetRevision;

use super::model::ResolvedJobTarget;

pub(in crate::features::operations) fn build_compare_authorization(
    target: &ResolvedJobTarget,
    origin: CompareOrigin,
) -> Result<CompareAuthorization, String> {
    CompareAuthorization::new(
        JobTargetRevision::new(
            target.registered_job.job_id.clone(),
            target.config_revision.clone(),
            target.target_index,
        )?,
        origin,
    )
}
