//! Seeding generated job files into a job directory, under the same lock as every other mutation.
//!
//! `gen-jobs` writes files a user is about to edit and the registry is about to read. Doing that
//! with a bare `fs::write` put it outside the two guarantees the rest of this module provides:
//! the advisory lock that serializes job-directory mutations, and the staged write that makes a
//! job file appear whole or not at all. A crash mid-write left a truncated TOML the loader then
//! refused, and an `exists()` check followed by a separate write is a TOCTOU — two generators, or
//! a generator racing a desktop save, could each see "absent" and the later one silently win.
//!
//! `commit_noreplace` is what makes "existing jobs are left alone" a property of the filesystem
//! rather than of the check that preceded it.

use std::path::{Path, PathBuf};

use crate::job::persistence::codec::staged_text;
use crate::job::persistence::registry::{lock_job_mutations, registered_job_path_in};

/// What a single seed attempt did.
#[derive(Debug, Eq, PartialEq)]
pub enum SeedOutcome {
    /// The file did not exist and now holds exactly `text`.
    Written,
    /// A file was already there and `overwrite` was not requested, so it was left untouched.
    Kept,
}

/// Write one generated job file into `dir`, holding the job-mutation lock for the whole attempt.
///
/// With `overwrite` false a name already present is kept and reported, and the refusal comes from
/// the atomic no-replace commit rather than from a prior existence check, so a job created between
/// the two can never be discarded.
pub fn seed_job_file(
    dir: &Path,
    name: &str,
    text: &str,
    overwrite: bool,
) -> std::io::Result<(PathBuf, SeedOutcome)> {
    let _lock = lock_job_mutations(dir)?;
    let path = registered_job_path_in(dir, name)?;
    let staged = staged_text(&path, text)?;
    if overwrite {
        staged.commit()?;
        return Ok((path, SeedOutcome::Written));
    }
    match staged.commit_noreplace() {
        Ok(()) => Ok((path, SeedOutcome::Written)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok((path, SeedOutcome::Kept))
        }
        Err(error) => Err(error),
    }
}
