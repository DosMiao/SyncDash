//! Projecting a completed job mutation onto its wire shape.
//!
//! The status-delivery warnings are the reason this is worth a test: a save that succeeded but
//! could not notify every listener must report both facts, and dropping the warnings on the way
//! out would present a partially-delivered save as a clean one.

use syncdash::job;

use crate::contracts::jobs::JobSaveDto;

pub(crate) fn job_save_dto(
    saved: job::SavedJob,
    status_delivery_warnings: Vec<String>,
) -> JobSaveDto {
    JobSaveDto {
        job_id: saved.job_id,
        name: saved.name,
        config_revision: saved.config_revision,
        effect: saved.effect,
        previous_name: saved.previous_name,
        status_delivery_warnings,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use syncdash::job::{JobMutationEffect, SavedJob};

    use super::*;

    #[test]
    fn committed_save_response_preserves_status_delivery_warnings() {
        let response = job_save_dto(
            SavedJob {
                name: "archive".into(),
                path: PathBuf::from("archive.toml"),
                job_id: "job-a".into(),
                config_revision: "revision-b".into(),
                effect: JobMutationEffect::Updated,
                previous_name: None,
            },
            vec!["compare-execution-status: listener unavailable".into()],
        );
        assert_eq!(response.effect, JobMutationEffect::Updated);
        assert_eq!(
            response.status_delivery_warnings,
            vec!["compare-execution-status: listener unavailable"]
        );
    }
}
