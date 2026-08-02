//! Changing which roots a job points at.
//!
//! Root edits are the most dangerous job mutation: a job whose target moved silently mirrors into
//! somewhere new. Each one re-reads and re-verifies the file under the lock after mutating the
//! in-memory copy, so a concurrent edit is refused rather than merged.

//! Atomic, identity- and revision-fenced mutations of registered jobs.

use super::fence::*;
use std::path::Path;

use crate::job::model::{Job, SCHEMA};
use crate::job::persistence::codec::{file_schema_at, staged_job};
use crate::job::persistence::registry::{
    lock_job_mutations, registered_job_path_in, validate_job_id,
};
use crate::job::persistence::types::{JobMutationEffect, JobRootField, JobRootMutation, SavedJob};
use crate::job::revision::config_revision;

pub(super) fn mutate_job_roots_in<F>(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
    mutate: F,
) -> std::io::Result<JobRootMutation>
where
    F: FnOnce(&mut Job) -> std::io::Result<()>,
{
    validate_job_id(expected_job_id)?;
    let _lock = lock_job_mutations(dir)?;
    let path = registered_job_path_in(dir, name)?;
    let mut current = load_expected_job(&path, name, expected_job_id, expected_config_revision)?;
    require_target(&current, target_index)?;
    mutate(&mut current)?;
    current.schema = SCHEMA;
    current.validate().map_err(invalid_job)?;
    let config_revision = config_revision(&current).map_err(invalid_job)?;
    let effect = if config_revision == expected_config_revision && file_schema_at(&path)? == SCHEMA
    {
        JobMutationEffect::NoOp
    } else {
        let staged = staged_job(&path, &current)?;
        load_expected_job(&path, name, expected_job_id, expected_config_revision)?;
        staged.commit()?;
        JobMutationEffect::Updated
    };
    Ok(JobRootMutation {
        mutation: SavedJob {
            name: name.to_string(),
            path,
            job_id: current.job_id,
            config_revision,
            effect,
            previous_name: None,
        },
        source: current.source,
        targets: current.targets,
    })
}

pub fn update_job_root(
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
    field: JobRootField,
    value: &str,
) -> std::io::Result<JobRootMutation> {
    update_job_root_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
        field,
        value,
    )
}

pub(super) fn update_job_root_in(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
    field: JobRootField,
    value: &str,
) -> std::io::Result<JobRootMutation> {
    if value.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "root value cannot be empty",
        ));
    }
    mutate_job_roots_in(
        dir,
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
        |current| {
            match field {
                JobRootField::Source => current.source = value.to_string(),
                JobRootField::Target => {
                    *target_mut(current, target_index) = value.to_string();
                }
            }
            Ok(())
        },
    )
}

pub fn swap_job_roots(
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
) -> std::io::Result<JobRootMutation> {
    swap_job_roots_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
    )
}

pub(super) fn swap_job_roots_in(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
) -> std::io::Result<JobRootMutation> {
    mutate_job_roots_in(
        dir,
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
        |current| {
            let previous_source = std::mem::take(&mut current.source);
            let previous_target =
                std::mem::replace(target_mut(current, target_index), previous_source);
            current.source = previous_target;
            Ok(())
        },
    )
}
