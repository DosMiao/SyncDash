//! Proving a job file is still the one the caller reviewed, before it is rewritten.
//!
//! Every mutation carries the job id and config revision the caller believed current. Checking
//! them under the directory lock is what stops a save from overwriting an edit made between the
//! read and the write — the job file is the whole description of what a run will do, so a lost
//! edit is a run doing something nobody asked for.

//! Atomic, identity- and revision-fenced mutations of registered jobs.

use std::path::Path;

use crate::job::model::Job;
use crate::job::persistence::codec::load_path;
use crate::job::persistence::registry::validate_job_id;
use crate::job::revision::config_revision;

pub(super) fn invalid_job(reason: String) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("invalid job: {reason}"),
    )
}

pub(super) fn current_revision_at(path: &Path) -> std::io::Result<String> {
    let (_, current) = load_path(path)?;
    config_revision(&current).map_err(invalid_job)
}

pub(super) fn require_revision(path: &Path, name: &str, expected: &str) -> std::io::Result<()> {
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name}"),
        ));
    }
    let current = current_revision_at(path)?;
    if current != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' changed on disk (expected revision {expected}, found {current}) — reload before saving"
            ),
        ));
    }
    Ok(())
}

pub(super) fn load_expected_job(
    path: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
) -> std::io::Result<Job> {
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name}"),
        ));
    }
    let (_, current) = load_path(path)?;
    validate_job_id(&current.job_id).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("bad job file {}: {error}", path.display()),
        )
    })?;
    if current.job_id != expected_job_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' was replaced since this editor loaded it (expected job_id '{expected_job_id}', found '{}') — reload before saving",
                current.job_id
            ),
        ));
    }
    let current_revision = config_revision(&current).map_err(invalid_job)?;
    if current_revision != expected_config_revision {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' changed on disk (expected revision {expected_config_revision}, found {current_revision}) — reload before saving"
            ),
        ));
    }
    Ok(current)
}

pub(super) fn rename_without_overwrite(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::hard_link(source, destination)?;
    if let Err(error) = std::fs::remove_file(source) {
        if let Err(rollback) = std::fs::remove_file(destination) {
            return Err(std::io::Error::new(
                error.kind(),
                format!(
                    "cannot remove the old job name: {error}; removing the collision-safe link also failed: {rollback}"
                ),
            ));
        }
        return Err(error);
    }
    Ok(())
}

pub(super) fn require_target(job: &Job, target_index: usize) -> std::io::Result<()> {
    if target_index >= job.targets.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "target_index {target_index} is out of range for {} target(s)",
                job.targets.len()
            ),
        ));
    }
    Ok(())
}

pub(super) fn target_mut(job: &mut Job, target_index: usize) -> &mut String {
    &mut job.targets[target_index]
}
