//! Descriptor-relative access to one local filesystem root.
//!
//! The counterpart of `fs::staged`, split by whose files they touch: everything under a user's
//! synced root mutates through these handles, which resolve each path segment against a retained
//! root descriptor so no engine path can address outside the root it was opened against.
//! SyncDash's own state files use `fs::staged` instead. The per-OS rename/flush primitives here
//! (`directory.rs`) are deliberately not shared with that stack — they answer to handle-relative
//! APIs — so a platform fact fixed in one stack must be checked against the other's assumptions.

use std::ffi::OsStr;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_primitives::fs as capability_fs;

use crate::foundation::names::TEMP_PREFIX;
use crate::foundation::path::{EntryName, RootRelativeDir, RootRelativePath};

mod directory;
mod identity;
mod staged;

use identity::{file_identity, random_token, segments, split_parent, EntryNameOsStr};

#[derive(Clone)]
pub struct LocalRoot {
    directory: LocalDirectory,
    display_path: Arc<PathBuf>,
}

#[derive(Clone)]
pub(crate) struct LocalDirectory {
    handle: Arc<std::fs::File>,
}

#[derive(Debug)]
pub(crate) struct LocalEntry {
    pub(crate) name: EntryName,
    pub(crate) metadata: capability_fs::Metadata,
}

/// One directory read, with the names the platform returned but Unicode cannot spell kept
/// separate. Such a name has no faithful table rel — a lossy substitution would point at a file
/// that does not exist — so it is skipped, never respelled, and each caller decides how loudly
/// to say so.
#[derive(Debug)]
pub(crate) struct DirectoryListing {
    pub(crate) entries: Vec<LocalEntry>,
    pub(crate) invalid_names: Vec<std::ffi::OsString>,
}

pub struct LocalStagedFile {
    parent: LocalDirectory,
    temporary_name: EntryName,
    destination_name: EntryName,
    file: Option<std::fs::File>,
    committed: bool,
    sync_parent_on_commit: bool,
}

impl LocalRoot {
    /// `open_ambient_dir` supplies the Windows directory handle sharing mode required to keep a
    /// renamed root from being silently retargeted through its old ambient path.
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let handle = capability_fs::open_ambient_dir(&path, cap_primitives::ambient_authority())?;
        Ok(Self {
            directory: LocalDirectory::new(handle),
            display_path: Arc::new(path),
        })
    }

    /// Creating the selected root is ambient authority; the returned object begins confinement at
    /// the descriptor opened after creation, so a later namespace substitution cannot retarget it.
    pub fn create(path: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&path)?;
        Self::open(path)
    }

    /// This spelling is presentation and volume-probing data only; child I/O never resolves it.
    pub fn display_path(&self) -> &Path {
        self.display_path.as_ref()
    }

    pub(crate) fn open_directory(
        &self,
        relative: &RootRelativeDir,
    ) -> std::io::Result<LocalDirectory> {
        let mut directory = self.directory.clone();
        for segment in segments(relative.as_str()) {
            directory = directory.open_subdirectory(OsStr::new(segment))?;
        }
        Ok(directory)
    }

    pub fn metadata_path(
        &self,
        relative: &RootRelativePath,
    ) -> std::io::Result<capability_fs::Metadata> {
        let (parent, name) = self.open_parent(relative)?;
        parent.metadata(name.as_os_str())
    }

    pub fn metadata_directory(
        &self,
        relative: &RootRelativeDir,
    ) -> std::io::Result<capability_fs::Metadata> {
        self.open_directory(relative)?.metadata_self()
    }

    pub(crate) fn read_directory(
        &self,
        relative: &RootRelativeDir,
    ) -> std::io::Result<DirectoryListing> {
        self.open_directory(relative)?.read_entries()
    }

    pub fn read_directory_names(
        &self,
        relative: &RootRelativeDir,
    ) -> std::io::Result<Vec<EntryName>> {
        self.read_directory(relative).map(|listing| {
            listing
                .entries
                .into_iter()
                .map(|entry| entry.name)
                .collect()
        })
    }

    pub fn open_read(&self, relative: &RootRelativePath) -> std::io::Result<std::fs::File> {
        let (parent, name) = self.open_parent(relative)?;
        parent.open_read(name.as_os_str())
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn is_dataless_file(&self, relative: &RootRelativePath) -> std::io::Result<bool> {
        use std::os::macos::fs::MetadataExt;
        const SF_DATALESS: u32 = 0x4000_0000;

        Ok(self.open_read(relative)?.metadata()?.st_flags() & SF_DATALESS != 0)
    }

    pub fn read(&self, relative: &RootRelativePath) -> std::io::Result<Vec<u8>> {
        let mut file = self.open_read(relative)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn read_to_string(&self, relative: &RootRelativePath) -> std::io::Result<String> {
        let mut file = self.open_read(relative)?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    }

    pub fn read_range(
        &self,
        relative: &RootRelativePath,
        offset: u64,
        length: usize,
    ) -> std::io::Result<Vec<u8>> {
        let mut file = self.open_read(relative)?;
        file.seek(std::io::SeekFrom::Start(offset))?;
        let mut bytes = vec![0; length];
        let mut read = 0;
        while read < bytes.len() {
            let count = file.read(&mut bytes[read..])?;
            if count == 0 {
                break;
            }
            read += count;
        }
        bytes.truncate(read);
        Ok(bytes)
    }

    pub fn read_link(&self, relative: &RootRelativePath) -> std::io::Result<PathBuf> {
        let (parent, name) = self.open_parent(relative)?;
        parent.read_link(name.as_os_str())
    }

    pub fn create_directory_all(&self, relative: &RootRelativeDir) -> std::io::Result<()> {
        let mut directory = self.directory.clone();
        for segment in segments(relative.as_str()) {
            let name = OsStr::new(segment);
            match directory.open_subdirectory(name) {
                Ok(child) => directory = child,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match directory.create_directory(name) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                    directory = directory.open_subdirectory(name)?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn create_directory_new(&self, relative: &RootRelativeDir) -> std::io::Result<()> {
        let (parent, name) = self.open_directory_parent(relative)?;
        parent.create_directory(name.as_os_str())
    }

    pub fn create_staged(&self, relative: &RootRelativePath) -> std::io::Result<LocalStagedFile> {
        let (parent, destination_name) = self.open_parent(relative)?;
        LocalStagedFile::create(parent, destination_name)
    }

    pub(crate) fn create_regular_file_new(
        &self,
        relative: &RootRelativePath,
    ) -> std::io::Result<std::fs::File> {
        let (parent, name) = self.open_parent(relative)?;
        parent.create_read_write_file(name.as_os_str())
    }

    pub fn open_append(&self, relative: &RootRelativePath) -> std::io::Result<std::fs::File> {
        let (parent, name) = self.open_parent(relative)?;
        parent.open_append(&name)
    }

    pub fn open_lock_file(&self, relative: &RootRelativePath) -> std::io::Result<std::fs::File> {
        let (parent, name) = self.open_parent(relative)?;
        parent.open_lock_file(name.as_os_str())
    }

    pub fn remove_file(&self, relative: &RootRelativePath) -> std::io::Result<()> {
        let (parent, name) = self.open_parent(relative)?;
        parent.remove_file_force(name.as_os_str())
    }

    pub fn remove_open_file(
        &self,
        relative: &RootRelativePath,
        opened_file: &std::fs::File,
    ) -> std::io::Result<()> {
        let (parent, name) = self.open_parent(relative)?;
        let expected_identity = file_identity(opened_file)?;
        for _ in 0..1024 {
            let claimed_name =
                EntryName::try_from(format!("{TEMP_PREFIX}remove.{}", random_token()?))
                    .expect("generated removal names satisfy the entry-name contract");
            match parent.rename_noreplace(name.as_os_str(), &parent, claimed_name.as_os_str()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
            let claimed_identity = parent
                .open_read(claimed_name.as_os_str())
                .and_then(|file| file_identity(&file));
            match claimed_identity {
                Ok(identity) if identity == expected_identity => {
                    return match parent.remove_file_force(claimed_name.as_os_str()) {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            Err(parent.rollback_claimed_file(&claimed_name, &name, error))
                        }
                    };
                }
                Ok(_) => {
                    return Err(parent.rollback_claimed_file(
                        &claimed_name,
                        &name,
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "file name changed before removal",
                        ),
                    ));
                }
                Err(error) => {
                    return Err(parent.rollback_claimed_file(&claimed_name, &name, error));
                }
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "could not reserve an identity-checked removal name for {:?}",
                relative.as_str()
            ),
        ))
    }

    pub fn remove_directory(&self, relative: &RootRelativeDir) -> std::io::Result<()> {
        let (parent, name) = self.open_directory_parent(relative)?;
        parent.remove_directory_force(name.as_os_str())
    }

    pub fn remove_directory_all(&self, relative: &RootRelativeDir) -> std::io::Result<()> {
        let (parent, name) = self.open_directory_parent(relative)?;
        let directory = parent.open_subdirectory(name.as_os_str())?;
        directory.remove_contents()?;

        let opened_identity = directory.identity()?;
        let current_identity = parent.identity_of(name.as_os_str())?;
        if opened_identity != current_identity {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "directory name changed while recursive removal was in progress",
            ));
        }
        drop(directory);
        parent.remove_directory_force(name.as_os_str())
    }

    pub fn rename(
        &self,
        source: &RootRelativePath,
        destination: &RootRelativePath,
    ) -> std::io::Result<()> {
        let (source_parent, source_name) = self.open_parent(source)?;
        let (destination_parent, destination_name) = self.open_parent(destination)?;
        source_parent.rename_force(
            source_name.as_os_str(),
            &destination_parent,
            destination_name.as_os_str(),
        )
    }

    pub fn rename_noreplace(
        &self,
        source: &RootRelativePath,
        destination: &RootRelativePath,
    ) -> std::io::Result<()> {
        self.rename_to_noreplace(source, self, destination)
    }

    pub fn rename_to_noreplace(
        &self,
        source: &RootRelativePath,
        destination_root: &LocalRoot,
        destination: &RootRelativePath,
    ) -> std::io::Result<()> {
        let (source_parent, source_name) = self.open_parent(source)?;
        let (destination_parent, destination_name) = destination_root.open_parent(destination)?;
        source_parent.rename_noreplace(
            source_name.as_os_str(),
            &destination_parent,
            destination_name.as_os_str(),
        )
    }

    pub fn set_mtime(&self, relative: &RootRelativePath, mtime_ms: i64) -> std::io::Result<()> {
        let (parent, name) = self.open_parent(relative)?;
        parent.set_mtime(name.as_os_str(), mtime_ms)
    }

    #[cfg(unix)]
    pub fn set_mode(&self, relative: &RootRelativePath, mode: u32) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let file = self.open_read(relative)?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
    }

    pub fn make_symlink(&self, relative: &RootRelativePath, target: &Path) -> std::io::Result<()> {
        let (parent, name) = self.open_parent(relative)?;
        parent.make_symlink(name.as_os_str(), target)
    }

    pub fn sync_parent(&self, relative: &RootRelativePath) -> std::io::Result<()> {
        let (parent, _) = self.open_parent(relative)?;
        parent.sync_all()
    }

    fn open_parent(
        &self,
        relative: &RootRelativePath,
    ) -> std::io::Result<(LocalDirectory, EntryName)> {
        let (parent, name) = split_parent(relative.as_str());
        Ok((self.open_directory(&parent)?, name))
    }

    fn open_directory_parent(
        &self,
        relative: &RootRelativeDir,
    ) -> std::io::Result<(LocalDirectory, EntryName)> {
        if relative.as_str().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the retained local root itself cannot be removed",
            ));
        }
        let (parent, name) = split_parent(relative.as_str());
        Ok((self.open_directory(&parent)?, name))
    }
}

#[cfg(test)]
mod tests;
