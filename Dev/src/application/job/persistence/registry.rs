//! Registered-job discovery, stable identity materialization, and direct-path CLI loading.

use std::path::{Path, PathBuf};

use crate::job::model::Job;
use crate::job::persistence::codec::{file_schema_at, load_path, staged_text};

/// Resolve a job name from the registry or a direct path to a `.toml` file.
pub fn resolve_path(name_or_path: &str) -> std::io::Result<PathBuf> {
    let path = PathBuf::from(name_or_path);
    if path.is_file() {
        return Ok(path);
    }
    let candidate = crate::foundation::dirs::jobs_dir().join(format!("{name_or_path}.toml"));
    if !candidate.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "job not found: {name_or_path} (looked at {})",
                candidate.display()
            ),
        ));
    }
    Ok(candidate)
}

/// Resolve a registered job name without accepting a path-shaped alias. Desktop IPC always deals
/// in names returned by `load_all`; direct paths remain a CLI-only convenience through `load`.
fn registered_job_path(name: &str) -> std::io::Result<PathBuf> {
    if name.is_empty()
        || name.trim() != name
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':'])
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid registered job name: {name}"),
        ));
    }
    Ok(crate::foundation::dirs::jobs_dir().join(format!("{name}.toml")))
}

fn existing_registered_job_path(name: &str) -> std::io::Result<PathBuf> {
    let path = registered_job_path(name)?;
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name} (looked at {})", path.display()),
        ));
    }
    Ok(path)
}

/// Return the schema declared on disk before migration. A missing schema key means v1.
pub fn file_schema(name_or_path: &str) -> std::io::Result<u32> {
    file_schema_at(&resolve_path(name_or_path)?)
}

/// Read the on-disk schema for a job registered in the jobs directory.
pub fn file_schema_named(name: &str) -> std::io::Result<u32> {
    file_schema_at(&existing_registered_job_path(name)?)
}

/// Load either a direct TOML path or a registered job name.
pub fn load(name_or_path: &str) -> std::io::Result<(String, Job)> {
    let path = PathBuf::from(name_or_path);
    if path.is_file() {
        load_path(&path)
    } else {
        load_named(name_or_path)
    }
}

/// Load exactly one job registered in the jobs directory. Unlike `load`, this never interprets the
/// argument as a path, so an IPC caller cannot substitute an old same-stem file from elsewhere.
pub fn load_named(name: &str) -> std::io::Result<(String, Job)> {
    let dir = crate::foundation::dirs::jobs_dir();
    let _lock = lock_job_mutations(&dir)?;
    let path = registered_job_path_in(&dir, name)?;
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name} (looked at {})", path.display()),
        ));
    }
    load_registered_path(&path)
}

pub fn load_all() -> std::io::Result<Vec<(String, Job)>> {
    let dir = crate::foundation::dirs::jobs_dir();
    let _lock = lock_job_mutations(&dir)?;
    let mut jobs = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            jobs.push(load_registered_path(&path)?);
        }
    }
    jobs.sort_by(|left, right| left.0.cmp(&right.0));

    let mut identities = std::collections::BTreeMap::new();
    for (name, job) in &jobs {
        if let Some(previous) = identities.insert(job.job_id.as_str(), name.as_str()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "jobs '{previous}' and '{name}' carry the same job_id '{}' — each registered job must have a unique identity",
                    job.job_id
                ),
            ));
        }
    }
    Ok(jobs)
}

/// Resolve a registered job by its stable identity rather than its mutable filename.
pub fn load_by_id(job_id: &str) -> std::io::Result<(String, Job)> {
    validate_job_id(job_id)?;
    load_all()?
        .into_iter()
        .find(|(_, job)| job.job_id == job_id)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("job identity not found: {job_id}"),
            )
        })
}

pub(super) fn validate_job_id(job_id: &str) -> std::io::Result<()> {
    if crate::foundation::token::is_lower_hex(job_id, crate::foundation::token::TOKEN_HEX_LEN) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "job_id must be {} lowercase hexadecimal characters",
                crate::foundation::token::TOKEN_HEX_LEN
            ),
        ))
    }
}

pub(super) fn new_job_id() -> std::io::Result<String> {
    crate::foundation::token::random_token_128()
        .map_err(|error| std::io::Error::other(format!("cannot create a job identity: {error}")))
}

/// Materialize identity metadata without serializing the whole job. Prepending one top-level key
/// preserves comments and leaves an old schema visible until the user explicitly saves it.
pub(super) fn load_registered_path(path: &Path) -> std::io::Result<(String, Job)> {
    let (name, mut job) = load_path(path)?;
    if job.job_id.is_empty() {
        let text = std::fs::read_to_string(path)?;
        let parsed: toml::Value = toml::from_str(&text).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad job file {}: {error}", path.display()),
            )
        })?;
        if parsed.get("job_id").is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad job file {}: job_id cannot be empty", path.display()),
            ));
        }
        job.job_id = new_job_id()?;
        let with_identity = format!("job_id = \"{}\"\n{text}", job.job_id);
        staged_text(path, &with_identity)?.commit()?;
    } else {
        validate_job_id(&job.job_id).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("bad job file {}: {error}", path.display()),
            )
        })?;
    }
    Ok((name, job))
}

pub(super) struct JobMutationLock {
    _file: std::fs::File,
}

pub(super) fn lock_job_mutations(dir: &Path) -> std::io::Result<JobMutationLock> {
    std::fs::create_dir_all(dir)?;
    // The lock file carries no content; it is opened only to hold an advisory lock. Truncation is
    // named explicitly so a concurrent holder's open descriptor is never emptied out from under it.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(dir.join(".syncdash-jobs.lock"))?;
    file.lock()?;
    Ok(JobMutationLock { _file: file })
}

pub(super) fn registered_job_path_in(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    registered_job_path(name).map(|path| {
        path.file_name()
            .map(|file_name| dir.join(file_name))
            .unwrap_or(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_job_names_cannot_be_reinterpreted_as_paths() {
        for invalid in [
            "../photos",
            "folder/photos",
            r"folder\photos",
            "/tmp/photos",
            "C:photos",
        ] {
            let error = registered_job_path(invalid).unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidInput,
                "{invalid}: {error}"
            );
        }
        let path = registered_job_path("photos.archive").unwrap();
        assert_eq!(path.file_name().unwrap(), "photos.archive.toml");
        assert_eq!(path.parent().unwrap(), crate::foundation::dirs::jobs_dir());
    }
}
