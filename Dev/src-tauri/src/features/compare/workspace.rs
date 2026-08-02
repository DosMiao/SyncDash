//! Reconciling a retained Compare workspace against the job it was produced from.
//!
//! A retained result outlives the job configuration that produced it, so every entry point has to
//! answer the same question: is the job still there, still configured the same way, and does the
//! target it was compared against still exist? Those three answers decide whether the workspace is
//! current, merely viewable, or gone — and the distinction is what keeps an immutable result
//! viewable after its job changed without letting it be executed.

use syncdash::job;

use crate::contracts::compare::CompareIdentity;
use crate::features::compare::evidence::repository::CompareWorkspaceJobState;
use crate::features::jobs::target::resolve_target;

/// Classify the job behind a retained result: current, reconfigured, or deleted.
pub(crate) fn job_state_for(
    compare_identity: &CompareIdentity,
) -> Result<CompareWorkspaceJobState, String> {
    match job::load_by_id(&compare_identity.job_id) {
        Ok((job_name, full_job)) => {
            let config_revision = job::config_revision(&full_job)
                .map_err(|error| format!("Job '{job_name}': {error}"))?;
            if config_revision != compare_identity.config_revision {
                return Ok(CompareWorkspaceJobState::ConfigurationChanged);
            }
            // A target that no longer resolves makes the workspace unusable even when the
            // revision matches, so this must run before reporting the job as current.
            resolve_target(&full_job, Some(compare_identity.target_index))?;
            Ok(CompareWorkspaceJobState::Current { job_name })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(CompareWorkspaceJobState::Deleted)
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Refuse a restore whose job changed while the request was in flight.
///
/// The frontend sends the revision it believed current when the user chose. Without this fence a
/// slow click restores a workspace against a configuration nobody reviewed.
pub(crate) fn require_expected_config_revision(
    job_name: &str,
    expected_config_revision: &str,
    current_config_revision: &str,
) -> Result<(), String> {
    if current_config_revision != expected_config_revision {
        return Err(format!("Job '{job_name}' changed before its Compare workspace could be restored — refresh the job and try again"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_revision_rejects_a_delayed_selection_request() {
        assert!(require_expected_config_revision("Archive", "revision-a", "revision-a").is_ok());
        let error =
            require_expected_config_revision("Archive", "revision-a", "revision-b").unwrap_err();
        assert!(error.contains("changed before its Compare workspace could be restored"));
    }
}
