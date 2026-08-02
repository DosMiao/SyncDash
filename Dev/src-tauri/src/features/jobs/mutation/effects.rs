use syncdash::job;

use crate::features::autoscan::controller::AutoScanController;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::store::OperationAuthorizationStore;

use super::model::{JobMutationStatusEvents, SavedJobMutationFacts};

fn mutation_facts(saved: &job::SavedJob, expected_revision: Option<&str>) -> SavedJobMutationFacts {
    SavedJobMutationFacts {
        renamed: saved.previous_name.is_some(),
        configuration_changed: expected_revision
            .is_some_and(|revision| revision != saved.config_revision),
    }
}

pub(crate) fn apply_saved_job_side_effects(
    saved: &job::SavedJob,
    expected_revision: Option<&str>,
    results: &CompareResultRepository,
    autoscan: &AutoScanController,
    authorizations: &OperationAuthorizationStore,
) -> Result<JobMutationStatusEvents, String> {
    let mutation = mutation_facts(saved, expected_revision);
    if mutation.renamed {
        results
            .rebind_job_name(&saved.job_id, &saved.name)
            .map_err(|error| error.to_string())?;
    }
    if mutation.configuration_changed {
        authorizations.revoke_job_authority(&saved.job_id);
    }
    let rename_status = mutation
        .renamed
        .then(|| autoscan.rebind_job_name(&saved.job_id, &saved.name))
        .flatten();
    let autoscan = if mutation.configuration_changed {
        autoscan.stop_if_job_id(&saved.job_id)
    } else {
        rename_status
    };
    let compare_execution = if mutation.configuration_changed {
        let previous_revision =
            expected_revision.expect("a semantic job update must carry its previous revision");
        results.expire_revision(
            &saved.job_id,
            previous_revision,
            crate::contracts::compare::CompareExecutionExpiryReasonDto::JobChanged,
        )
    } else {
        Vec::new()
    };
    Ok(JobMutationStatusEvents {
        autoscan,
        compare_execution,
    })
}

pub(crate) fn delete_job_side_effects(
    job_id: &str,
    results: &CompareResultRepository,
    autoscan: &AutoScanController,
    authorizations: &OperationAuthorizationStore,
) -> JobMutationStatusEvents {
    authorizations.revoke_job_authority(job_id);
    JobMutationStatusEvents {
        autoscan: autoscan.stop_if_job_id(job_id),
        compare_execution: results.expire_job(job_id),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use syncdash::job::{JobMutationEffect, SavedJob};

    use super::*;

    fn saved_job(
        effect: JobMutationEffect,
        config_revision: &str,
        previous_name: Option<&str>,
    ) -> SavedJob {
        SavedJob {
            name: "archive".into(),
            path: PathBuf::from("archive.toml"),
            job_id: "job-a".into(),
            config_revision: config_revision.into(),
            effect,
            previous_name: previous_name.map(str::to_string),
        }
    }

    #[test]
    fn authority_tracks_revision_change_independently_from_rename() {
        let renamed_and_changed =
            saved_job(JobMutationEffect::Renamed, "revision-b", Some("photos"));
        assert_eq!(
            mutation_facts(&renamed_and_changed, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: true,
                configuration_changed: true
            }
        );
        let pure_rename = saved_job(JobMutationEffect::Renamed, "revision-a", Some("photos"));
        assert_eq!(
            mutation_facts(&pure_rename, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: true,
                configuration_changed: false
            }
        );
        let semantic_update = saved_job(JobMutationEffect::Updated, "revision-b", None);
        assert_eq!(
            mutation_facts(&semantic_update, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: false,
                configuration_changed: true
            }
        );
        let no_op = saved_job(JobMutationEffect::NoOp, "revision-a", None);
        assert_eq!(
            mutation_facts(&no_op, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: false,
                configuration_changed: false
            }
        );
        let schema_only_update = saved_job(JobMutationEffect::Updated, "revision-a", None);
        assert_eq!(
            mutation_facts(&schema_only_update, Some("revision-a")),
            SavedJobMutationFacts {
                renamed: false,
                configuration_changed: false
            }
        );
    }
}
