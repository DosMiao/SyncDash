//! Handle-relative directory operations and no-replace namespace transactions.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(windows)]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
#[cfg(windows)]
use cap_primitives::fs::OpenOptionsExt as _;
use cap_primitives::fs::{self as capability_fs, OpenOptions};

use crate::foundation::path::EntryName;

use super::identity::{file_identity, EntryNameOsStr, FileIdentity};
use super::{LocalDirectory, LocalEntry, LocalStagedFile};

impl LocalDirectory {
    pub(super) fn new(handle: std::fs::File) -> Self {
        Self {
            handle: Arc::new(handle),
        }
    }

    pub(super) fn metadata_self(&self) -> std::io::Result<capability_fs::Metadata> {
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

    pub(super) fn open_append(&self, name: &EntryName) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options.append(true).create(true).follow(FollowSymlinks::No);
        capability_fs::open(&self.handle, Path::new(name.as_os_str()), &options)
    }

    pub(super) fn open_lock_file(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
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

    pub(super) fn metadata(&self, name: &OsStr) -> std::io::Result<capability_fs::Metadata> {
        capability_fs::stat(&self.handle, Path::new(name), FollowSymlinks::No)
    }

    pub(super) fn read_entries(&self) -> std::io::Result<Vec<LocalEntry>> {
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

    pub(super) fn open_subdirectory(&self, name: &OsStr) -> std::io::Result<Self> {
        capability_fs::open_dir_nofollow(&self.handle, Path::new(name)).map(Self::new)
    }

    pub(super) fn open_read(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
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

    pub(super) fn create_file(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        capability_fs::open(&self.handle, Path::new(name), &options)
    }

    pub(super) fn create_read_write_file(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        capability_fs::open(&self.handle, Path::new(name), &options)
    }

    pub(super) fn read_link(&self, name: &OsStr) -> std::io::Result<PathBuf> {
        capability_fs::read_link_contents(&self.handle, Path::new(name))
    }

    pub(super) fn create_directory(&self, name: &OsStr) -> std::io::Result<()> {
        capability_fs::create_dir(
            &self.handle,
            Path::new(name),
            &capability_fs::DirOptions::new(),
        )
    }

    pub(super) fn remove_file(&self, name: &OsStr) -> std::io::Result<()> {
        capability_fs::remove_file(&self.handle, Path::new(name))
    }

    pub(super) fn remove_file_force(&self, name: &OsStr) -> std::io::Result<()> {
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

    pub(super) fn rollback_claimed_file(
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

    pub(super) fn remove_directory(&self, name: &OsStr) -> std::io::Result<()> {
        capability_fs::remove_dir(&self.handle, Path::new(name))
    }

    pub(super) fn remove_directory_force(&self, name: &OsStr) -> std::io::Result<()> {
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
    pub(super) fn rename(
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
    pub(super) fn rename(
        &self,
        source_name: &OsStr,
        destination_parent: &Self,
        destination_name: &OsStr,
    ) -> std::io::Result<()> {
        self.rename_windows(source_name, destination_parent, destination_name, true)
    }

    pub(super) fn rename_force(
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
    pub(super) fn rename_noreplace(
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
    pub(super) fn rename_noreplace(
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
    pub(super) fn rename_noreplace(
        &self,
        source_name: &OsStr,
        destination_parent: &Self,
        destination_name: &OsStr,
    ) -> std::io::Result<()> {
        self.rename_windows(source_name, destination_parent, destination_name, false)
    }

    #[cfg(windows)]
    pub(super) fn rename_windows(
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
    pub(super) fn rename_noreplace(
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
    pub(super) fn clear_readonly(
        &self,
        name: &OsStr,
        metadata: capability_fs::Metadata,
    ) -> std::io::Result<()> {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(false);
        capability_fs::set_permissions(&self.handle, Path::new(name), permissions)
    }

    pub(super) fn set_mtime(&self, name: &OsStr, mtime_ms: i64) -> std::io::Result<()> {
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
    pub(super) fn make_symlink(&self, name: &OsStr, target: &Path) -> std::io::Result<()> {
        capability_fs::symlink_contents(target, &self.handle, Path::new(name))
    }

    #[cfg(windows)]
    pub(super) fn make_symlink(&self, name: &OsStr, target: &Path) -> std::io::Result<()> {
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

    pub(super) fn sync_all(&self) -> std::io::Result<()> {
        self.handle.sync_all()
    }

    pub(super) fn identity(&self) -> std::io::Result<FileIdentity> {
        file_identity(&self.handle.metadata()?)
    }

    pub(super) fn identity_of(&self, name: &OsStr) -> std::io::Result<FileIdentity> {
        let child = self.open_subdirectory(name)?;
        child.identity()
    }

    pub(super) fn remove_contents(&self) -> std::io::Result<()> {
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
