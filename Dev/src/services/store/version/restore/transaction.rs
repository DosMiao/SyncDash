use std::path::Path;

use crate::foundation::path::{RootRelativeDir, RootRelativePath};
use crate::fs::local_root::{LocalRoot, LocalStagedFile};
use crate::fs::lock::RootLock;

use super::super::{
    check_version_lease, hash_local_file, invalid_data, parent_directory, relative_directory,
    relative_path, PreservedEntry, READ_CHUNK,
};
use super::manifest::{verify_reverse_delta, RestorePayload, ValidatedRestoreEntry};

struct StagedRestoreSymlink {
    root: LocalRoot,
    temporary: RootRelativePath,
    published: bool,
}

impl StagedRestoreSymlink {
    fn publish(mut self, destination: &RootRelativePath, lease: &RootLock) -> std::io::Result<()> {
        check_version_lease(lease)?;
        self.root.rename_noreplace(&self.temporary, destination)?;
        self.published = true;
        self.root.sync_parent(destination)
    }
}

impl Drop for StagedRestoreSymlink {
    fn drop(&mut self) {
        if !self.published {
            let _ = self.root.remove_file(&self.temporary);
        }
    }
}

enum StagedRestoreEntry {
    RegularFile(LocalStagedFile),
    Symlink(StagedRestoreSymlink),
}

impl StagedRestoreEntry {
    fn publish(self, destination: &RootRelativePath, lease: &RootLock) -> std::io::Result<()> {
        check_version_lease(lease)?;
        match self {
            Self::RegularFile(staged) => staged.commit_noreplace(),
            Self::Symlink(staged) => staged.publish(destination, lease),
        }
    }
}

fn temporary_symlink_path(destination: &RootRelativePath) -> std::io::Result<RootRelativePath> {
    let name = format!(
        "{}restore.{}",
        crate::foundation::names::TEMP_PREFIX,
        crate::fs::vfs::random_name_token().map_err(std::io::Error::from)?
    );
    match crate::foundation::path::parent(destination.as_str()) {
        Some(parent) => relative_path(format!("{parent}/{name}")),
        None => relative_path(name),
    }
}

fn stage_restore_symlink(
    root: &LocalRoot,
    entry: &PreservedEntry,
    relative: &RootRelativePath,
    target: &Path,
    lease: &RootLock,
) -> std::io::Result<StagedRestoreEntry> {
    for _ in 0..1024 {
        check_version_lease(lease)?;
        let temporary = temporary_symlink_path(relative)?;
        match root.make_symlink(&temporary, target) {
            Ok(()) => {
                let staged = StagedRestoreSymlink {
                    root: root.clone(),
                    temporary,
                    published: false,
                };
                check_version_lease(lease)?;
                staged
                    .root
                    .set_mtime(&staged.temporary, entry.old_mtime_ms)?;
                #[cfg(unix)]
                if let Some(expected_mode) = entry.old_mode {
                    use cap_primitives::fs::MetadataExt;
                    let actual_mode = staged.root.metadata_path(&staged.temporary)?.mode() & 0o7777;
                    if actual_mode != expected_mode {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Unsupported,
                            format!(
                                "restored symlink mode for {:?} would be {actual_mode:o}, expected {expected_mode:o}",
                                relative.as_str()
                            ),
                        ));
                    }
                }
                #[cfg(not(unix))]
                if entry.old_mode.is_some() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        format!(
                            "this platform cannot reproduce the recorded symlink mode for {:?}",
                            relative.as_str()
                        ),
                    ));
                }
                return Ok(StagedRestoreEntry::Symlink(staged));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "could not reserve a temporary symlink name for {:?}",
            relative.as_str()
        ),
    ))
}

fn set_staged_restore_metadata(
    staged: &LocalStagedFile,
    entry: &PreservedEntry,
    _relative: &RootRelativePath,
) -> std::io::Result<()> {
    staged.set_mtime(entry.old_mtime_ms)?;
    #[cfg(unix)]
    if let Some(mode) = entry.old_mode {
        staged.set_mode(mode)?;
    }
    #[cfg(not(unix))]
    if entry.old_mode.is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "this platform cannot reproduce the recorded permissions for {:?}",
                _relative.as_str()
            ),
        ));
    }
    Ok(())
}

fn stage_restore_regular_file(
    root: &LocalRoot,
    entry: &PreservedEntry,
    relative: &RootRelativePath,
    archive: &RootRelativePath,
    lease: &RootLock,
) -> std::io::Result<StagedRestoreEntry> {
    let mut source = root.open_read(archive)?;
    let mut staged = root.create_staged(relative)?;
    let mut hasher = blake3::Hasher::new();
    let mut copied = 0u64;
    let mut buffer = vec![0u8; READ_CHUNK as usize];
    loop {
        use std::io::Read;
        check_version_lease(lease)?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        staged.write_all_from(&mut &buffer[..count])?;
        copied = copied.checked_add(count as u64).ok_or_else(|| {
            invalid_data(format!(
                "restored byte count overflows for {:?}",
                relative.as_str()
            ))
        })?;
        hasher.update(&buffer[..count]);
    }
    if copied != entry.old_size || hasher.finalize().to_hex().as_str() != entry.old_hash {
        return Err(invalid_data(format!(
            "whole-file payload changed while staging {:?}",
            relative.as_str()
        )));
    }
    check_version_lease(lease)?;
    set_staged_restore_metadata(&staged, entry, relative)?;
    staged.seal(true)?;
    Ok(StagedRestoreEntry::RegularFile(staged))
}

fn stage_restore_reverse_delta(
    root: &LocalRoot,
    entry: &PreservedEntry,
    relative: &RootRelativePath,
    blob: &RootRelativePath,
    lease: &RootLock,
) -> std::io::Result<StagedRestoreEntry> {
    let mut staged = root.create_staged(relative)?;
    verify_reverse_delta(
        root,
        entry,
        relative,
        blob,
        || check_version_lease(lease),
        |bytes| {
            staged.write_all_from(&mut &bytes[..])?;
            Ok(())
        },
    )?;
    check_version_lease(lease)?;
    set_staged_restore_metadata(&staged, entry, relative)?;
    staged.seal(true)?;
    Ok(StagedRestoreEntry::RegularFile(staged))
}

fn stage_restore_entry(
    root: &LocalRoot,
    validated: &ValidatedRestoreEntry,
    lease: &RootLock,
) -> std::io::Result<StagedRestoreEntry> {
    check_version_lease(lease)?;
    let destination = &validated.entry.relative_path;
    root.create_directory_all(&parent_directory(destination))?;
    match &validated.payload {
        RestorePayload::RegularFile { archive } => {
            stage_restore_regular_file(root, &validated.entry, destination, archive, lease)
        }
        RestorePayload::Symlink { target } => {
            stage_restore_symlink(root, &validated.entry, destination, target, lease)
        }
        RestorePayload::ReverseDelta { blob } => {
            stage_restore_reverse_delta(root, &validated.entry, destination, blob, lease)
        }
    }
}

fn create_restore_session(root: &LocalRoot, lease: &RootLock) -> std::io::Result<RootRelativeDir> {
    let base = relative_directory(format!("{}/restore", crate::foundation::names::APP_DIR))?;
    check_version_lease(lease)?;
    root.create_directory_all(&base)?;
    for _ in 0..1024 {
        let token = crate::fs::vfs::random_name_token().map_err(std::io::Error::from)?;
        let session = relative_directory(format!("{}/{token}", base.as_str()))?;
        check_version_lease(lease)?;
        match root.create_directory_new(&session) {
            Ok(()) => return Ok(session),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not reserve a restore-retention directory",
    ))
}

fn retained_restore_path(
    session: &RootRelativeDir,
    relative: &RootRelativePath,
) -> std::io::Result<RootRelativePath> {
    relative_path(format!("{}/{}", session.as_str(), relative.as_str()))
}

fn sync_rename_parents(
    root: &LocalRoot,
    source: &RootRelativePath,
    destination: &RootRelativePath,
) -> std::io::Result<()> {
    root.sync_parent(destination)?;
    if crate::foundation::path::parent(source.as_str())
        != crate::foundation::path::parent(destination.as_str())
    {
        root.sync_parent(source)?;
    }
    Ok(())
}

fn rollback_displaced_entry(
    root: &LocalRoot,
    retained: &RootRelativePath,
    destination: &RootRelativePath,
    primary: std::io::Error,
    lease: &RootLock,
    retained_destination_count: &mut u64,
) -> std::io::Error {
    if let Err(lock_error) = check_version_lease(lease) {
        return std::io::Error::new(
            primary.kind(),
            format!(
                "{primary}; {lock_error}; displaced content remains recoverable at {:?}",
                retained.as_str()
            ),
        );
    }
    match root.rename_noreplace(retained, destination) {
        Ok(()) => {
            *retained_destination_count = retained_destination_count.saturating_sub(1);
            match sync_rename_parents(root, retained, destination) {
                Ok(()) => primary,
            Err(sync_error) => std::io::Error::new(
                primary.kind(),
                format!(
                    "{primary}; displaced content was restored to {:?}, but rollback durability failed ({sync_error})",
                    destination.as_str()
                ),
            ),
            }
        }
        Err(rollback_error) => std::io::Error::new(
            primary.kind(),
            format!(
                "{primary}; rollback failed ({rollback_error}); displaced content remains recoverable at {:?}",
                retained.as_str()
            ),
        ),
    }
}

fn restored_destination_matches(
    root: &LocalRoot,
    validated: &ValidatedRestoreEntry,
) -> std::io::Result<bool> {
    let destination = &validated.entry.relative_path;
    let metadata = match root.metadata_path(destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match &validated.payload {
        RestorePayload::Symlink { target } => {
            if !metadata.file_type().is_symlink() {
                return Ok(false);
            }
            Ok(root.read_link(destination)? == *target)
        }
        RestorePayload::RegularFile { .. } | RestorePayload::ReverseDelta { .. } => Ok(metadata
            .is_file()
            && metadata.len() == validated.entry.old_size
            && hash_local_file(root, destination)? == validated.entry.old_hash),
    }
}

pub(super) fn restore_entry(
    root: &LocalRoot,
    validated: &ValidatedRestoreEntry,
    lease: &RootLock,
    retention_session: &mut Option<RootRelativeDir>,
    retained_destination_count: &mut u64,
) -> std::io::Result<()> {
    let staged = stage_restore_entry(root, validated, lease)?;
    let destination = &validated.entry.relative_path;
    let retained = match root.metadata_path(destination) {
        Ok(_) => {
            if retention_session.is_none() {
                *retention_session = Some(create_restore_session(root, lease)?);
            }
            let retained = retained_restore_path(
                retention_session.as_ref().expect("created above"),
                destination,
            )?;
            check_version_lease(lease)?;
            root.create_directory_all(&parent_directory(&retained))?;
            check_version_lease(lease)?;
            match root.rename_noreplace(destination, &retained) {
                Ok(()) => {
                    *retained_destination_count += 1;
                    if let Err(error) = sync_rename_parents(root, destination, &retained) {
                        return Err(rollback_displaced_entry(
                            root,
                            &retained,
                            destination,
                            error,
                            lease,
                            retained_destination_count,
                        ));
                    }
                    Some(retained)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    if let Err(error) = staged.publish(destination, lease) {
        match root.metadata_path(destination) {
            Ok(_) => {
                let exact = match restored_destination_matches(root, validated) {
                    Ok(exact) => exact,
                    Err(inspection_error) => {
                        return Err(std::io::Error::new(
                            error.kind(),
                            format!(
                                "restore publication failed ({error}); destination exists, but intended content could not be verified ({inspection_error}){}",
                                retained
                                    .as_ref()
                                    .map(|path| format!(
                                        "; displaced content remains recoverable at {:?}",
                                        path.as_str()
                                    ))
                                    .unwrap_or_default()
                            ),
                        ));
                    }
                };
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "restore publication failed ({error}); destination now exists{}{}",
                        if exact {
                            " with the intended content"
                        } else {
                            ""
                        },
                        retained
                            .as_ref()
                            .map(|path| format!(
                                "; displaced content remains recoverable at {:?}",
                                path.as_str()
                            ))
                            .unwrap_or_default()
                    ),
                ));
            }
            Err(stat_error) if stat_error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(retained) = retained {
                    return Err(rollback_displaced_entry(
                        root,
                        &retained,
                        destination,
                        error,
                        lease,
                        retained_destination_count,
                    ));
                }
            }
            Err(stat_error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "restore publication failed ({error}) and destination state could not be determined ({stat_error}){}",
                        retained
                            .as_ref()
                            .map(|path| format!(
                                "; displaced content remains recoverable at {:?}",
                                path.as_str()
                            ))
                            .unwrap_or_default()
                    ),
                ));
            }
        }
        return Err(error);
    }
    Ok(())
}
