//! Creating and updating a whole job file.

//! Atomic, identity- and revision-fenced mutations of registered jobs.

use super::fence::*;
use std::path::Path;

use crate::job::model::{Job, SCHEMA};
use crate::job::persistence::codec::{file_schema_at, staged_job};
use crate::job::persistence::registry::{
    load_registered_path, lock_job_mutations, new_job_id, registered_job_path_in,
};
use crate::job::persistence::types::{JobMutationEffect, SavedJob};
use crate::job::revision::config_revision;

/// Create, update, or rename one registered job without overwriting an unseen revision.
pub fn save_job(
    name: &str,
    job: &Job,
    original_name: Option<&str>,
    expected_revision: Option<&str>,
) -> std::io::Result<SavedJob> {
    job.validate().map_err(invalid_job)?;
    let job = Job {
        schema: SCHEMA,
        ..job.clone()
    };
    let config_revision = config_revision(&job).map_err(invalid_job)?;
    save_job_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        &job,
        original_name,
        expected_revision,
        config_revision,
    )
}

pub(super) fn save_job_in(
    dir: &Path,
    name: &str,
    job: &Job,
    original_name: Option<&str>,
    expected_revision: Option<&str>,
    config_revision: String,
) -> std::io::Result<SavedJob> {
    let _lock = lock_job_mutations(dir)?;
    let destination = registered_job_path_in(dir, name)?;
    let mut persisted = job.clone();
    let mut effect = JobMutationEffect::Created;
    let mut previous_name = None;
    match (original_name, expected_revision) {
        (None, None) => {
            if !persisted.job_id.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a new job must not supply job_id; the registry assigns a fresh identity",
                ));
            }
            if destination.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("job '{name}' already exists — reload it before saving"),
                ));
            }
            persisted.job_id = new_job_id()?;
            let staged = staged_job(&destination, &persisted)?;
            if destination.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("job '{name}' was created while this save was being prepared"),
                ));
            }
            staged.commit()?;
        }
        (Some(original_name), Some(expected_revision)) => {
            let original = registered_job_path_in(dir, original_name)?;
            require_revision(&original, original_name, expected_revision)?;
            let (_, current) = load_registered_path(&original)?;
            if persisted.job_id != current.job_id {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "job '{original_name}' was replaced since this editor loaded it (expected job_id '{}', found '{}') — reload before saving",
                        persisted.job_id, current.job_id
                    ),
                ));
            }
            if original == destination {
                if config_revision == expected_revision && file_schema_at(&original)? == SCHEMA {
                    return Ok(SavedJob {
                        name: name.to_string(),
                        path: destination,
                        job_id: persisted.job_id,
                        config_revision,
                        effect: JobMutationEffect::NoOp,
                        previous_name: None,
                    });
                }
                effect = JobMutationEffect::Updated;
                let staged = staged_job(&destination, &persisted)?;
                require_revision(&original, original_name, expected_revision)?;
                staged.commit()?;
            } else {
                effect = JobMutationEffect::Renamed;
                previous_name = Some(original_name.to_string());
                if destination.exists() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("cannot rename job '{original_name}' to '{name}': destination already exists"),
                    ));
                }
                let staged = staged_job(&destination, &persisted)?;
                require_revision(&original, original_name, expected_revision)?;
                if destination.exists() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("job '{name}' was created while this rename was being prepared"),
                    ));
                }
                rename_without_overwrite(&original, &destination)?;
                if let Err(error) = staged.commit() {
                    if let Err(rollback) = rename_without_overwrite(&destination, &original) {
                        return Err(std::io::Error::new(
                            error.kind(),
                            format!("cannot save renamed job: {error}; restoring the original name also failed: {rollback}"),
                        ));
                    }
                    return Err(error);
                }
            }
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "original_name and expected_revision must be supplied together for an update",
            ));
        }
    }
    Ok(SavedJob {
        name: name.to_string(),
        path: destination,
        job_id: persisted.job_id,
        config_revision,
        effect,
        previous_name,
    })
}
