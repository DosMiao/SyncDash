//! Descriptor-relative access to one local filesystem root.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(windows)]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
#[cfg(windows)]
use cap_primitives::fs::OpenOptionsExt as _;
use cap_primitives::fs::{self as capability_fs, OpenOptions};

use crate::foundation::names::TEMP_PREFIX;
use crate::foundation::path::{EntryName, RootRelativeDir, RootRelativePath};

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
    ) -> std::io::Result<Vec<LocalEntry>> {
        self.open_directory(relative)?.read_entries()
    }

    pub fn read_directory_names(
        &self,
        relative: &RootRelativeDir,
    ) -> std::io::Result<Vec<EntryName>> {
        self.read_directory(relative)
            .map(|entries| entries.into_iter().map(|entry| entry.name).collect())
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
        let expected_identity = file_identity(&opened_file.metadata()?)?;
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
                .and_then(|file| file.metadata())
                .and_then(|metadata| file_identity(&metadata));
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

impl LocalDirectory {
    fn new(handle: std::fs::File) -> Self {
        Self {
            handle: Arc::new(handle),
        }
    }

    fn metadata_self(&self) -> std::io::Result<capability_fs::Metadata> {
        capability_fs::Metadata::from_file(&self.handle)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn try_clone_handle(&self) -> std::io::Result<std::fs::File> {
        self.handle.try_clone()
    }

    pub(crate) fn create_child_directory(&self, name: &EntryName) -> std::io::Result<Self> {
        self.create_directory(name.as_os_str())?;
        self.open_subdirectory(name.as_os_str())
    }

    pub(crate) fn create_new_file(&self, name: &EntryName) -> std::io::Result<std::fs::File> {
        self.create_file(name.as_os_str())
    }

    fn open_append(&self, name: &EntryName) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options.append(true).create(true).follow(FollowSymlinks::No);
        capability_fs::open(&self.handle, Path::new(name.as_os_str()), &options)
    }

    fn open_lock_file(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .follow(FollowSymlinks::No);
        let file = capability_fs::open(&self.handle, Path::new(name), &options)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "lock path is not a regular file",
            ));
        }
        Ok(file)
    }

    pub(crate) fn create_staged(&self, name: EntryName) -> std::io::Result<LocalStagedFile> {
        LocalStagedFile::create(self.clone(), name)
    }

    fn metadata(&self, name: &OsStr) -> std::io::Result<capability_fs::Metadata> {
        capability_fs::stat(&self.handle, Path::new(name), FollowSymlinks::No)
    }

    fn read_entries(&self) -> std::io::Result<Vec<LocalEntry>> {
        let mut entries = Vec::new();
        for entry in capability_fs::read_base_dir(&self.handle)? {
            let entry = entry?;
            let os_name = entry.file_name();
            let name = os_name.into_string().map_err(|name| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("directory entry name is not valid Unicode: {name:?}"),
                )
            })?;
            let name = EntryName::try_from(name).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
            })?;
            let metadata = self.metadata(name.as_os_str())?;
            entries.push(LocalEntry { name, metadata });
        }
        Ok(entries)
    }

    fn open_subdirectory(&self, name: &OsStr) -> std::io::Result<Self> {
        capability_fs::open_dir_nofollow(&self.handle, Path::new(name)).map(Self::new)
    }

    fn open_read(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = capability_fs::open(&self.handle, Path::new(name), &options)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expected a regular file",
            ));
        }
        Ok(file)
    }

    fn create_file(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        capability_fs::open(&self.handle, Path::new(name), &options)
    }

    fn create_read_write_file(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        capability_fs::open(&self.handle, Path::new(name), &options)
    }

    fn read_link(&self, name: &OsStr) -> std::io::Result<PathBuf> {
        capability_fs::read_link_contents(&self.handle, Path::new(name))
    }

    fn create_directory(&self, name: &OsStr) -> std::io::Result<()> {
        capability_fs::create_dir(
            &self.handle,
            Path::new(name),
            &capability_fs::DirOptions::new(),
        )
    }

    fn remove_file(&self, name: &OsStr) -> std::io::Result<()> {
        capability_fs::remove_file(&self.handle, Path::new(name))
    }

    fn remove_file_force(&self, name: &OsStr) -> std::io::Result<()> {
        match self.remove_file(name) {
            #[cfg(windows)]
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                let metadata = self.metadata(name)?;
                if metadata.is_symlink() {
                    return Err(error);
                }
                self.clear_readonly(name, metadata)?;
                self.remove_file(name)
            }
            result => result,
        }
    }

    fn rollback_claimed_file(
        &self,
        claimed_name: &EntryName,
        original_name: &EntryName,
        primary: std::io::Error,
    ) -> std::io::Error {
        match self.rename_noreplace(
            claimed_name.as_os_str(),
            self,
            original_name.as_os_str(),
        ) {
            Ok(()) => primary,
            Err(rollback_error) => std::io::Error::new(
                primary.kind(),
                format!(
                    "{primary}; rollback to {:?} failed ({rollback_error}); the claimed entry remains recoverable as {:?}",
                    original_name.as_str(),
                    claimed_name.as_str()
                ),
            ),
        }
    }

    fn remove_directory(&self, name: &OsStr) -> std::io::Result<()> {
        capability_fs::remove_dir(&self.handle, Path::new(name))
    }

    fn remove_directory_force(&self, name: &OsStr) -> std::io::Result<()> {
        match self.remove_directory(name) {
            #[cfg(windows)]
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                let metadata = self.metadata(name)?;
                self.clear_readonly(name, metadata)?;
                self.remove_directory(name)
            }
            result => result,
        }
    }

    #[cfg(not(windows))]
    fn rename(
        &self,
        source_name: &OsStr,
        destination_parent: &Self,
        destination_name: &OsStr,
    ) -> std::io::Result<()> {
        capability_fs::rename(
            &self.handle,
            Path::new(source_name),
            &destination_parent.handle,
            Path::new(destination_name),
        )
    }

    #[cfg(windows)]
    fn rename(
        &self,
        source_name: &OsStr,
        destination_parent: &Self,
        destination_name: &OsStr,
    ) -> std::io::Result<()> {
        self.rename_windows(source_name, destination_parent, destination_name, true)
    }

    fn rename_force(
        &self,
        source_name: &OsStr,
        destination_parent: &Self,
        destination_name: &OsStr,
    ) -> std::io::Result<()> {
        match self.rename(source_name, destination_parent, destination_name) {
            #[cfg(windows)]
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                let metadata = self.metadata(source_name)?;
                if metadata.is_symlink() {
                    return Err(error);
                }
                self.clear_readonly(source_name, metadata)?;
                self.rename(source_name, destination_parent, destination_name)
            }
            result => result,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn rename_noreplace(
        &self,
        source_name: &OsStr,
        destination_parent: &Self,
        destination_name: &OsStr,
    ) -> std::io::Result<()> {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        let source_name = CString::new(source_name.as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "source name contains NUL")
        })?;
        let destination_name = CString::new(destination_name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "destination name contains NUL",
            )
        })?;
        let result = unsafe {
            libc::renameat2(
                self.handle.as_raw_fd(),
                source_name.as_ptr(),
                destination_parent.handle.as_raw_fd(),
                destination_name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "macos")]
    fn rename_noreplace(
        &self,
        source_name: &OsStr,
        destination_parent: &Self,
        destination_name: &OsStr,
    ) -> std::io::Result<()> {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        let source_name = CString::new(source_name.as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "source name contains NUL")
        })?;
        let destination_name = CString::new(destination_name.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "destination name contains NUL",
            )
        })?;
        let result = unsafe {
            libc::renameatx_np(
                self.handle.as_raw_fd(),
                source_name.as_ptr(),
                destination_parent.handle.as_raw_fd(),
                destination_name.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(windows)]
    fn rename_noreplace(
        &self,
        source_name: &OsStr,
        destination_parent: &Self,
        destination_name: &OsStr,
    ) -> std::io::Result<()> {
        self.rename_windows(source_name, destination_parent, destination_name, false)
    }

    #[cfg(windows)]
    fn rename_windows(
        &self,
        source_name: &OsStr,
        destination_parent: &Self,
        destination_name: &OsStr,
        replace_existing: bool,
    ) -> std::io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
        };

        const DELETE_ACCESS: u32 = 0x0001_0000;
        const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
        const SHARE_READ: u32 = 0x0000_0001;
        const SHARE_WRITE: u32 = 0x0000_0002;
        const SHARE_DELETE: u32 = 0x0000_0004;

        let mut options = OpenOptions::new();
        options
            .access_mode(DELETE_ACCESS | SYNCHRONIZE_ACCESS)
            .share_mode(SHARE_READ | SHARE_WRITE | SHARE_DELETE)
            .maybe_dir(true)
            .follow(FollowSymlinks::No);
        let source = capability_fs::open(&self.handle, Path::new(source_name), &options)?;
        let destination_name: Vec<u16> = destination_name.encode_wide().collect();
        let bytes = destination_name
            .len()
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "destination name is too long",
                )
            })?;
        let allocation = std::mem::size_of::<FILE_RENAME_INFO>()
            .checked_add(bytes.saturating_sub(std::mem::size_of::<u16>()))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "destination name is too long",
                )
            })?;
        let words = allocation.div_ceil(std::mem::size_of::<usize>());
        let mut storage = vec![0usize; words];
        let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*information).Anonymous.ReplaceIfExists = replace_existing;
            (*information).RootDirectory = destination_parent.handle.as_raw_handle();
            (*information).FileNameLength = bytes as u32;
            std::ptr::copy_nonoverlapping(
                destination_name.as_ptr(),
                std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
                destination_name.len(),
            );
            if SetFileInformationByHandle(
                source.as_raw_handle(),
                FileRenameInfo,
                information.cast(),
                allocation as u32,
            ) == 0
            {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        windows
    )))]
    fn rename_noreplace(
        &self,
        _source_name: &OsStr,
        _destination_parent: &Self,
        _destination_name: &OsStr,
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "descriptor-relative exclusive rename is unavailable on this platform",
        ))
    }

    #[cfg(windows)]
    fn clear_readonly(
        &self,
        name: &OsStr,
        metadata: capability_fs::Metadata,
    ) -> std::io::Result<()> {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(false);
        capability_fs::set_permissions(&self.handle, Path::new(name), permissions)
    }

    fn set_mtime(&self, name: &OsStr, mtime_ms: i64) -> std::io::Result<()> {
        let duration = std::time::Duration::from_millis(mtime_ms.unsigned_abs());
        let time = if mtime_ms >= 0 {
            std::time::UNIX_EPOCH.checked_add(duration)
        } else {
            std::time::UNIX_EPOCH.checked_sub(duration)
        }
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "mtime is out of range")
        })?;
        capability_fs::set_times_nofollow(
            &self.handle,
            Path::new(name),
            None,
            Some(capability_fs::SystemTimeSpec::Absolute(
                cap_primitives::time::SystemTime::from_std(time),
            )),
        )
    }

    #[cfg(not(windows))]
    fn make_symlink(&self, name: &OsStr, target: &Path) -> std::io::Result<()> {
        capability_fs::symlink_contents(target, &self.handle, Path::new(name))
    }

    #[cfg(windows)]
    fn make_symlink(&self, name: &OsStr, target: &Path) -> std::io::Result<()> {
        if target.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "absolute symlink targets cannot be created within a confined local root on Windows",
            ));
        }
        capability_fs::symlink_file(target, &self.handle, Path::new(name)).or_else(|file_error| {
            capability_fs::symlink_dir(target, &self.handle, Path::new(name)).map_err(
                |directory_error| {
                    std::io::Error::new(
                        directory_error.kind(),
                        format!(
                            "file symlink creation failed ({file_error}); directory symlink creation failed ({directory_error})"
                        ),
                    )
                },
            )
        })
    }

    fn sync_all(&self) -> std::io::Result<()> {
        self.handle.sync_all()
    }

    fn identity(&self) -> std::io::Result<FileIdentity> {
        file_identity(&self.handle.metadata()?)
    }

    fn identity_of(&self, name: &OsStr) -> std::io::Result<FileIdentity> {
        let child = self.open_subdirectory(name)?;
        child.identity()
    }

    fn remove_contents(&self) -> std::io::Result<()> {
        let entries: Vec<OsString> = capability_fs::read_base_dir(&self.handle)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<_>>()?;
        for name in entries {
            let metadata = self.metadata(&name)?;
            if metadata.is_dir() {
                let child = self.open_subdirectory(&name)?;
                child.remove_contents()?;
                let opened_identity = child.identity()?;
                let current_identity = self.identity_of(&name)?;
                if opened_identity != current_identity {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "directory entry changed while recursive removal was in progress",
                    ));
                }
                drop(child);
                self.remove_directory_force(&name)?;
            } else {
                self.remove_file_force(&name)?;
            }
        }
        Ok(())
    }
}

impl LocalStagedFile {
    fn create(parent: LocalDirectory, destination_name: EntryName) -> std::io::Result<Self> {
        loop {
            let temporary_name =
                EntryName::try_from(format!("{TEMP_PREFIX}stage.{}", random_token()?))
                    .expect("generated staging names satisfy the entry-name contract");
            match parent.create_file(temporary_name.as_os_str()) {
                Ok(file) => {
                    return Ok(Self {
                        parent,
                        temporary_name,
                        destination_name,
                        file: Some(file),
                        committed: false,
                        sync_parent_on_commit: false,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub fn staged_len(&self) -> std::io::Result<u64> {
        match &self.file {
            Some(file) => Ok(file.metadata()?.len()),
            None => Ok(self.parent.metadata(self.temporary_name.as_os_str())?.len()),
        }
    }

    pub fn try_clone_file(&self) -> std::io::Result<std::fs::File> {
        match &self.file {
            Some(file) => file.try_clone(),
            None => self.parent.open_read(self.temporary_name.as_os_str()),
        }
    }

    pub fn set_mtime(&self, mtime_ms: i64) -> std::io::Result<()> {
        match &self.file {
            Some(file) => {
                let time = filetime::FileTime::from_unix_time(
                    mtime_ms.div_euclid(1000),
                    (mtime_ms.rem_euclid(1000) * 1_000_000) as u32,
                );
                filetime::set_file_handle_times(file, None, Some(time))
            }
            None => self
                .parent
                .set_mtime(self.temporary_name.as_os_str(), mtime_ms),
        }
    }

    #[cfg(unix)]
    pub fn set_mode(&self, mode: u32) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let file = self.try_clone_file()?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
    }

    pub fn write_all_from(&mut self, reader: &mut dyn Read) -> std::io::Result<u64> {
        std::io::copy(reader, self.file_mut()?)
    }

    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
        let file = self.file_mut()?;
        file.seek(std::io::SeekFrom::Start(offset))?;
        file.write_all(bytes)
    }

    pub fn set_len(&mut self, length: u64) -> std::io::Result<()> {
        self.file_mut()?.set_len(length)
    }

    pub fn seal(&mut self, fsync: bool) -> std::io::Result<()> {
        self.sync_parent_on_commit |= fsync;
        if let Some(mut file) = self.file.take() {
            file.flush()?;
            if fsync {
                file.sync_all()?;
            }
        }
        Ok(())
    }

    pub fn sync_file(&self) -> std::io::Result<()> {
        self.try_clone_file()?.sync_all()
    }

    pub fn commit(mut self) -> std::io::Result<()> {
        if self.file.is_some() {
            self.seal(true)?;
        }
        self.parent.rename(
            self.temporary_name.as_os_str(),
            &self.parent,
            self.destination_name.as_os_str(),
        )?;
        self.committed = true;
        if self.sync_parent_on_commit {
            self.parent.sync_all()?;
        }
        Ok(())
    }

    pub fn commit_noreplace(mut self) -> std::io::Result<()> {
        if self.file.is_some() {
            self.seal(true)?;
        }
        self.parent.rename_noreplace(
            self.temporary_name.as_os_str(),
            &self.parent,
            self.destination_name.as_os_str(),
        )?;
        self.committed = true;
        if self.sync_parent_on_commit {
            self.parent.sync_all()?;
        }
        Ok(())
    }

    fn file_mut(&mut self) -> std::io::Result<&mut std::fs::File> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file is already sealed"))
    }
}

impl Write for LocalStagedFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file is already sealed"))?
            .write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file is already sealed"))?
            .flush()
    }
}

impl Drop for LocalStagedFile {
    fn drop(&mut self) {
        if !self.committed {
            self.file.take();
            let _ = self.parent.remove_file(self.temporary_name.as_os_str());
        }
    }
}

trait EntryNameOsStr {
    fn as_os_str(&self) -> &OsStr;
}

impl EntryNameOsStr for EntryName {
    fn as_os_str(&self) -> &OsStr {
        OsStr::new(self.as_str())
    }
}

fn segments(relative: &str) -> impl Iterator<Item = &str> {
    relative.split('/').filter(|segment| !segment.is_empty())
}

fn split_parent(relative: &str) -> (RootRelativeDir, EntryName) {
    let (parent, name) = relative
        .rsplit_once('/')
        .map_or(("", relative), |(parent, name)| (parent, name));
    (
        RootRelativeDir::try_from(parent)
            .expect("validated root-relative paths have a valid parent directory"),
        EntryName::try_from(name)
            .expect("the final segment of a validated root-relative path is an entry name"),
    )
}

fn random_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| {
        std::io::Error::other(format!("random token generation failed: {error}"))
    })?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(token, "{byte:02x}").expect("writing into a String cannot fail");
    }
    Ok(token)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    file: u64,
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Ok(FileIdentity {
        device: metadata.dev(),
        file: metadata.ino(),
    })
}

#[cfg(windows)]
fn file_identity(metadata: &std::fs::Metadata) -> std::io::Result<FileIdentity> {
    use std::os::windows::fs::MetadataExt;
    Ok(FileIdentity {
        device: metadata.volume_serial_number().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "volume identity is unavailable for this directory handle",
            )
        })? as u64,
        file: metadata.file_index().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "file identity is unavailable for this directory handle",
            )
        })?,
    })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_metadata: &std::fs::Metadata) -> std::io::Result<FileIdentity> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stable directory identity is unavailable on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("syncdash-local-root-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn path(value: &str) -> RootRelativePath {
        RootRelativePath::try_from(value).unwrap()
    }

    fn directory(value: &str) -> RootRelativeDir {
        RootRelativeDir::try_from(value).unwrap()
    }

    #[test]
    fn traversal_never_reaches_the_capability_api() {
        assert!(RootRelativePath::try_from("../outside").is_err());
        assert!(RootRelativePath::try_from("safe/../../outside").is_err());
        assert!(RootRelativePath::try_from("/outside").is_err());
        assert!(RootRelativePath::try_from(r"C:\outside").is_err());
    }

    #[test]
    fn lock_file_is_created_with_read_and_write_access() {
        let root_path = test_directory("lock-file");
        let root = LocalRoot::open(root_path.clone()).unwrap();
        let lock_path = path("mutation.lock");
        let mut file = root.open_lock_file(&lock_path).unwrap();

        file.write_all(b"owner").unwrap();
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        let mut owner = String::new();
        file.read_to_string(&mut owner).unwrap();
        assert_eq!(owner, "owner");
        let second = root.open_lock_file(&lock_path).unwrap();
        assert!(second.metadata().unwrap().is_file());
        file.lock().unwrap();
        assert!(matches!(
            second.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        file.unlock().unwrap();

        let _ = std::fs::remove_dir_all(root_path);
    }

    #[test]
    fn new_regular_file_creation_is_exclusive_and_read_write() {
        let root_path = test_directory("new-regular-file");
        let root = LocalRoot::open(root_path.clone()).unwrap();
        let relative = path("package.tmp");
        let mut file = root.create_regular_file_new(&relative).unwrap();

        file.write_all(b"package").unwrap();
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "package");
        assert_eq!(
            root.create_regular_file_new(&relative).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );

        let _ = std::fs::remove_dir_all(root_path);
    }

    #[cfg(unix)]
    #[test]
    fn lock_file_never_follows_a_symlink() {
        use std::os::unix::fs::symlink;

        let root_path = test_directory("lock-symlink-root");
        let outside = test_directory("lock-symlink-outside");
        let outside_file = outside.join("outside.lock");
        std::fs::write(&outside_file, b"outside").unwrap();
        symlink(&outside_file, root_path.join("mutation.lock")).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        assert!(root.open_lock_file(&path("mutation.lock")).is_err());
        assert_eq!(std::fs::read(&outside_file).unwrap(), b"outside");

        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn intermediate_and_final_symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;

        let root_path = test_directory("symlink-refusal-root");
        let outside = test_directory("symlink-refusal-outside");
        std::fs::write(outside.join("sentinel"), b"outside").unwrap();
        symlink(&outside, root_path.join("redirect")).unwrap();
        symlink(outside.join("sentinel"), root_path.join("final-link")).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        assert!(root.read(&path("redirect/sentinel")).is_err());
        assert!(root.read(&path("final-link")).is_err());
        root.remove_file(&path("final-link")).unwrap();
        assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");

        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn read_link_returns_an_absolute_target_without_following_it() {
        use std::os::unix::fs::symlink;

        let root_path = test_directory("absolute-link-root");
        let outside = root_path.with_file_name(format!(
            "syncdash-local-root-absolute-target-{}",
            std::process::id()
        ));
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, root_path.join("link")).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        assert_eq!(root.read_link(&path("link")).unwrap(), outside);
        assert!(root.read(&path("link")).is_err());

        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[cfg(unix)]
    #[test]
    fn staged_commit_stays_with_the_parent_handle_after_name_substitution() {
        use std::os::unix::fs::symlink;

        let root_path = test_directory("staged-parent-root");
        let outside = test_directory("staged-parent-outside");
        std::fs::create_dir(root_path.join("parent")).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();
        let mut staged = root.create_staged(&path("parent/result")).unwrap();
        staged.write_all(b"confined").unwrap();

        std::fs::rename(root_path.join("parent"), root_path.join("detached")).unwrap();
        symlink(&outside, root_path.join("parent")).unwrap();
        staged.commit().unwrap();

        assert_eq!(
            std::fs::read(root_path.join("detached/result")).unwrap(),
            b"confined"
        );
        assert!(!outside.join("result").exists());
        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn direct_write_and_remove_refuse_an_intermediate_symlink_substitution() {
        use std::os::unix::fs::symlink;

        let root_path = test_directory("direct-parent-root");
        let outside = test_directory("direct-parent-outside");
        std::fs::create_dir(root_path.join("parent")).unwrap();
        std::fs::write(root_path.join("parent/victim"), b"inside").unwrap();
        std::fs::write(outside.join("victim"), b"outside").unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        std::fs::rename(root_path.join("parent"), root_path.join("detached")).unwrap();
        symlink(&outside, root_path.join("parent")).unwrap();

        assert!(root.open_append(&path("parent/new")).is_err());
        assert!(root.remove_file(&path("parent/victim")).is_err());
        assert!(!outside.join("new").exists());
        assert_eq!(std::fs::read(outside.join("victim")).unwrap(), b"outside");
        assert_eq!(
            std::fs::read(root_path.join("detached/victim")).unwrap(),
            b"inside"
        );
        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_parent_swaps_never_read_the_outside_sentinel() {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicBool, Ordering};

        let root_path = test_directory("swap-root");
        let outside = test_directory("swap-outside");
        std::fs::create_dir(root_path.join("safe")).unwrap();
        std::fs::write(outside.join("sentinel"), b"outside-secret").unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_root = root_path.clone();
        let thread_outside = outside.clone();
        let swapper = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                if std::fs::rename(thread_root.join("safe"), thread_root.join("held")).is_ok() {
                    if symlink(&thread_outside, thread_root.join("safe")).is_ok() {
                        let _ = std::fs::remove_file(thread_root.join("safe"));
                    }
                    let _ = std::fs::rename(thread_root.join("held"), thread_root.join("safe"));
                }
            }
        });

        for _ in 0..2_000 {
            if let Ok(bytes) = root.read(&path("safe/sentinel")) {
                assert_ne!(bytes, b"outside-secret");
            }
        }
        stop.store(true, Ordering::Relaxed);
        swapper.join().unwrap();

        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"outside-secret"
        );
        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn no_replace_commit_keeps_an_existing_destination() {
        let root_path = test_directory("no-replace");
        std::fs::write(root_path.join("destination"), b"existing").unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();
        let mut staged = root.create_staged(&path("destination")).unwrap();
        staged.write_all(b"replacement").unwrap();

        let error = staged.commit_noreplace().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(root_path.join("destination")).unwrap(),
            b"existing"
        );
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[test]
    fn identity_checked_removal_restores_a_different_current_entry() {
        let root_path = test_directory("identity-removal");
        std::fs::write(root_path.join("expected"), b"expected").unwrap();
        std::fs::write(root_path.join("current"), b"current").unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();
        let expected = root.open_read(&path("expected")).unwrap();

        let error = root
            .remove_open_file(&path("current"), &expected)
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(
            std::fs::read(root_path.join("current")).unwrap(),
            b"current"
        );
        assert_eq!(
            std::fs::read(root_path.join("expected")).unwrap(),
            b"expected"
        );
        assert!(std::fs::read_dir(&root_path).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(TEMP_PREFIX)));
        drop(expected);
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[test]
    fn no_replace_rename_moves_a_directory_as_one_entry_operation() {
        let root_path = test_directory("no-replace-directory");
        std::fs::create_dir(root_path.join("source-directory")).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        root.rename_noreplace(&path("source-directory"), &path("destination-directory"))
            .unwrap();
        assert!(!root_path.join("source-directory").exists());
        assert!(root_path.join("destination-directory").is_dir());
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[cfg(unix)]
    #[test]
    fn no_replace_rename_moves_a_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let root_path = test_directory("no-replace-symlink");
        std::fs::write(root_path.join("target"), b"target").unwrap();
        symlink("target", root_path.join("source-link")).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        root.rename_noreplace(&path("source-link"), &path("destination-link"))
            .unwrap();
        assert!(!root_path.join("source-link").exists());
        assert_eq!(
            std::fs::read_link(root_path.join("destination-link")).unwrap(),
            PathBuf::from("target")
        );
        assert_eq!(std::fs::read(root_path.join("target")).unwrap(), b"target");
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[cfg(unix)]
    #[test]
    fn recursive_remove_refuses_a_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let root_path = test_directory("recursive-root");
        let outside = test_directory("recursive-outside");
        std::fs::write(outside.join("sentinel"), b"outside").unwrap();
        symlink(&outside, root_path.join("tree")).unwrap();
        let root = LocalRoot::open(root_path.clone()).unwrap();

        assert!(root.remove_directory_all(&directory("tree")).is_err());
        assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
        let _ = std::fs::remove_dir_all(root_path);
        let _ = std::fs::remove_dir_all(outside);
    }
}
