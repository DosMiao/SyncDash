//! Launch-channel binding and last-moment evidence/registry reservation.

use crate::features::compare::evidence::repository::validation::validate_retained_compare;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::apply::ApplyAuthorizationKind;
use crate::features::operations::lifecycle::lease::{ActiveRunLease, RunCommandLease};
use crate::features::operations::lifecycle::model::RunPurpose;

use crate::features::operations::apply::preparation::model::PreparedApply;
use crate::features::operations::target::reload_prepared_target;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ApplyLaunch {
    Interactive { progress_launch_id: u64 },
    AutoScan,
}

impl ApplyLaunch {
    pub(super) fn bind(
        authorization_kind: ApplyAuthorizationKind,
        progress_launch_id: Option<u64>,
    ) -> Result<Self, String> {
        match (authorization_kind, progress_launch_id) {
            (ApplyAuthorizationKind::Interactive, Some(progress_launch_id)) => {
                Ok(Self::Interactive { progress_launch_id })
            }
            (ApplyAuthorizationKind::AutoScan, None) => Ok(Self::AutoScan),
            (ApplyAuthorizationKind::Interactive, None) => Err(
                "Interactive Apply requires its reserved progress window — review Apply again"
                    .into(),
            ),
            (ApplyAuthorizationKind::AutoScan, Some(_)) => {
                Err("AutoScan Apply cannot use an interactive progress-window launch".into())
            }
        }
    }
}

pub(super) fn revalidate_retained_before_apply(
    results: &CompareResultRepository,
    prepared: &PreparedApply,
    command: &RunCommandLease,
    launch: ApplyLaunch,
) -> Result<ActiveRunLease, String> {
    // Root/capability probes can be slow. Re-read the registry after them, without holding result,
    // authorization, or run locks, so an external TOML edit cannot enter the reservation through
    // stale in-memory state. The core reopens roots after reservation and enforces exact consent.
    let _current = reload_prepared_target(&prepared.target)?;
    let retained = results
        .get_exact(&prepared.owner.identity)
        .map_err(|error| error.to_string())?;
    validate_retained_compare(
        retained.as_ref(),
        &prepared.owner,
        &prepared.target.registered_job.job_id,
        &prepared.target.job_name,
        prepared.target.target_index,
        &prepared.target.config_revision,
        Some(&prepared.plan_digest),
    )?;
    results.with_fresh_execution_eligibility(&prepared.owner.identity, || match launch {
        ApplyLaunch::Interactive { progress_launch_id } => {
            command.start_apply_from_progress_launch(progress_launch_id)
        }
        ApplyLaunch::AutoScan => command.start_run(RunPurpose::Apply),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_authorization_kind_requires_the_matching_launch_channel() {
        assert_eq!(
            ApplyLaunch::bind(ApplyAuthorizationKind::Interactive, Some(17)).unwrap(),
            ApplyLaunch::Interactive {
                progress_launch_id: 17
            }
        );
        assert_eq!(
            ApplyLaunch::bind(ApplyAuthorizationKind::AutoScan, None).unwrap(),
            ApplyLaunch::AutoScan
        );
        assert!(ApplyLaunch::bind(ApplyAuthorizationKind::Interactive, None).is_err());
        assert!(ApplyLaunch::bind(ApplyAuthorizationKind::AutoScan, Some(17)).is_err());
    }
}
