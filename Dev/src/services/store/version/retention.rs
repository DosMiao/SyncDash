//! Listing retained versions and pruning the oldest.
//!
//! Pruning takes the version-root lease first and re-checks it before each removal: a prune that
//! keeps deleting after another process has taken the root would be removing that process's
//! history, not its own.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use super::content::{invalid_data, read_version_metadata, relative_directory, relative_path};
use super::model::VersionIndexEntry;
use crate::foundation::path::to_native;
use crate::fs::local_root::LocalRoot;
use crate::fs::lock::RootLock;
use crate::fs::vfs::local::LocalVfs;
use crate::fs::vfs::{Support, Vfs, VfsCaps};

pub fn list(root: &Path) -> std::io::Result<Vec<VersionIndexEntry>> {
    let local_root = LocalRoot::open(root.to_path_buf())?;
    list_from_local_root(&local_root)
}

pub(super) fn list_from_local_root(root: &LocalRoot) -> std::io::Result<Vec<VersionIndexEntry>> {
    let index_relative = relative_path(format!(
        "{}/index.jsonl",
        crate::foundation::names::VERSION_STORE_DIR
    ))?;
    let index_display = root.display_path().join(to_native(index_relative.as_str()));
    let bytes = match read_version_metadata(root, &index_relative, "version index") {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let text = String::from_utf8(bytes)
        .map_err(|error| invalid_data(format!("version index is not UTF-8: {error}")))?;
    let mut entries = Vec::new();
    let mut version_ids = HashSet::new();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: VersionIndexEntry = serde_json::from_str(line).map_err(|error| {
            invalid_data(format!(
                "invalid version index line {} in {}: {error}",
                line_index + 1,
                index_display.display()
            ))
        })?;
        if !version_ids.insert(entry.id.clone()) {
            return Err(invalid_data(format!(
                "duplicate version id {:?} on index line {}",
                entry.id,
                line_index + 1
            )));
        }
        entries.push(entry);
    }
    Ok(entries)
}

pub(super) struct LockedVersionRoot {
    pub(super) root: LocalRoot,
    pub(super) lease: RootLock,
    pub(super) caps: VfsCaps,
}

pub(super) fn acquire_version_root(path: &Path) -> std::io::Result<LockedVersionRoot> {
    let local_vfs = LocalVfs::open(path.to_path_buf()).map_err(std::io::Error::from)?;
    let caps = local_vfs.caps();
    if caps.exclusive_staged_file_publish != Support::Yes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "version mutation requires exclusive staged-file publication for its root lock",
        ));
    }
    let root = local_vfs.local_root().clone();
    let vfs: Arc<dyn Vfs> = Arc::new(local_vfs);
    let lease = RootLock::acquire_vfs(&vfs)?;
    Ok(LockedVersionRoot { root, lease, caps })
}

pub(super) fn check_version_lease(lease: &RootLock) -> std::io::Result<()> {
    if lease.verify_lease_identity() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "version mutation stopped because root-lock ownership was lost",
        ))
    }
}

/// Keep the newest `keep` versions and delete the rest. Returns the ids of the deleted versions.
pub fn prune(root: &Path, keep: usize) -> std::io::Result<Vec<String>> {
    let locked = acquire_version_root(root)?;
    let mut versions = list_from_local_root(&locked.root)?;
    if versions.is_empty() {
        return Ok(Vec::new());
    }
    versions.sort_by_key(|line| line.ts_ms);
    let prune_count = versions.len().saturating_sub(keep);
    let pruned: Vec<VersionIndexEntry> = versions.drain(..prune_count).collect();
    if pruned.is_empty() {
        return Ok(Vec::new());
    }
    let index_path = relative_path(format!(
        "{}/index.jsonl",
        crate::foundation::names::VERSION_STORE_DIR
    ))?;
    check_version_lease(&locked.lease)?;
    let mut staged = locked.root.create_staged(&index_path)?;
    for line in &versions {
        writeln!(staged, "{}", serde_json::to_string(line)?)?;
    }
    staged.seal(true)?;
    check_version_lease(&locked.lease)?;
    // Publish the logical deletion first: interruption may leave an unindexed orphan directory,
    // but the index never promises a version whose payload was already removed.
    staged.commit()?;

    for entry in &pruned {
        let version_directory = relative_directory(format!(
            "{}/{}",
            crate::foundation::names::VERSION_STORE_DIR,
            entry.id.as_str()
        ))?;
        check_version_lease(&locked.lease)?;
        match locked.root.remove_directory_all(&version_directory) {
            Ok(()) => {
                locked.root.sync_parent(&index_path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "version {:?} was removed from the index, but its orphaned directory could not be deleted ({error})",
                        entry.id.as_str()
                    ),
                ))
            }
        }
    }
    Ok(pruned
        .into_iter()
        .map(|entry| entry.id.into_string())
        .collect())
}
