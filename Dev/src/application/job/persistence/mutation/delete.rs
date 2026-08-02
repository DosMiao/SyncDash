//! Removing a registered job.

//! Atomic, identity- and revision-fenced mutations of registered jobs.

use super::fence::*;
use std::path::Path;

use crate::job::persistence::registry::{
    load_registered_path, lock_job_mutations, registered_job_path_in,
};
use crate::job::persistence::types::{DeletedJob, JobMutationEffect};

pub fn delete_job(
    name: &str,
    expected_job_id: &str,
    expected_revision: &str,
) -> std::io::Result<DeletedJob> {
    delete_job_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        expected_job_id,
        expected_revision,
    )
}

pub(super) fn delete_job_in(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_revision: &str,
) -> std::io::Result<DeletedJob> {
    let _lock = lock_job_mutations(dir)?;
    let path = registered_job_path_in(dir, name)?;
    require_revision(&path, name, expected_revision)?;
    let (_, current) = load_registered_path(&path)?;
    if current.job_id != expected_job_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' was replaced since this editor loaded it (expected job_id '{expected_job_id}', found '{}') — reload before deleting",
                current.job_id
            ),
        ));
    }
    std::fs::remove_file(path)?;
    Ok(DeletedJob {
        name: name.to_string(),
        job_id: current.job_id,
        config_revision: expected_revision.to_string(),
        effect: JobMutationEffect::Deleted,
    })
}
