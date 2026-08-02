//! The sync-mode archive: the record of what the two sides last agreed on.
//!
//! Refreshed after a successful apply, and never over paths that ended in conflict — an archive
//! that claims agreement where there was none turns the next run's conflict into a silent
//! overwrite.
//!
//! Reading is separate from replacement because the read path is used by Compare and by the CLI's
//! snapshot commands, which must never take the write lock.

mod paths;
mod publish;
mod refresh;
mod target;

#[cfg(test)]
mod tests;

pub use refresh::refresh_archive_with;

use self::target::ArchiveTarget;

use crate::model::table::TableArtifact;
use std::io::{self};
use std::path::Path;

pub fn load_archive(destination: &Path) -> io::Result<Option<TableArtifact>> {
    match ArchiveTarget::open_for_read(destination)? {
        Some(target) => {
            let lock = target.acquire_lock()?;
            target.load_or_migrate(&lock)
        }
        None => Ok(None),
    }
}
