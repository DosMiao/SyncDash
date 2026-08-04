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
use super::{DirectoryListing, LocalDirectory, LocalEntry, LocalStagedFile};

/// Placeholder body for a symlink claim. It is overwritten by the rename that follows within the
/// same call, and is only ever observable to a reader racing a publication in progress.
#[cfg(target_os = "macos")]
const CLAIM_LINK_TARGET: &[u8] = b".syncdash-claim\0";

/// The raw-syscall rename paths need owned NUL-terminated names. An embedded NUL is a caller bug
/// rather than a filesystem condition, so it is reported as invalid input instead of an OS error.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn nul_terminated(name: &OsStr, role: &str) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{role} name contains NUL"),
        )
    })
}

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

    pub(super) fn read_entries(&self) -> std::io::Result<DirectoryListing> {
        let mut entries = Vec::new();
        let mut invalid_names = Vec::new();
        for entry in capability_fs::read_base_dir(&self.handle)? {
            let entry = entry?;
            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                // The cross-platform contract for a name Unicode cannot spell is skip-and-count,
                // never substitute: a lossy respelling is a path that resolves to nothing, which
                // apply would miss and mirror would read as a deletion on the other side.
                Err(raw_name) => {
                    invalid_names.push(raw_name);
                    continue;
                }
            };
            let name = EntryName::try_from(name).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
            })?;
            let metadata = self.metadata(name.as_os_str())?;
            entries.push(LocalEntry { name, metadata });
        }
        Ok(DirectoryListing {
            entries,
            invalid_names,
        })
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

    #[cfg(windows)]
    pub(super) fn open_read_write(&self, name: &OsStr) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).follow(FollowSymlinks::No);
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

    /// `remove_file`, clearing the read-only attribute on a PermissionDenied retry — **on Windows**.
    ///
    /// Git marks loose objects `r--r--r--`, and Windows (plus SMB servers honoring the DOS
    /// attribute) refuses to delete such files — a real sync against a `.git`-carrying tree failed
    /// thousands of deletes with os error 5 exactly this way.
    ///
    /// The retry is Windows-only because it is only *correct* on Windows, where the read-only DOS
    /// attribute really is the cause and clearing it really is the remedy. It is tempting to think
    /// the retry simply never fires on unix, but that is wrong about the trigger: `unlink` returns
    /// EACCES when the **parent directory** is not writable, under a sticky-bit directory you do
    /// not own, or on a `chflags uchg` file — measured, errno 13. And on unix
    /// `set_readonly(false)` is not an attribute, it is `mode |= 0o222`: measured, 0600 becomes
    /// 0622. So in the commonest case the chmod succeeded (you own the file), the retry failed
    /// again (the parent is still not writable), and the file was left group- and world-writable
    /// permanently. Mirroring into `/Users/Shared` or another user's tree widened one more file on
    /// every failed delete.
    ///
    /// On unix the original PermissionDenied propagates untouched. That is both the loud failure
    /// and the honest diagnosis: the file's own mode was never the problem.
    ///
    /// A symlink stays failed rather than being retried: std has no `lchmod`, so clearing
    /// permissions would chmod the **target**.
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

    /// The directory half of [`Self::remove_file_force`], and a distinct case: on Windows the
    /// read-only attribute on a *directory* is nominally a "this folder is customized" flag, but
    /// `RemoveDirectory` still refuses it. Measured on Win11 26200: removing an empty directory
    /// carrying the attribute fails with `PermissionDenied` (os error 5), the same wording users
    /// saw on the read-only file case.
    ///
    /// Windows-only for the same reason as the file case: on unix the retry turned 0755 into 0777.
    ///
    /// This never becomes a recursive delete — a non-empty directory still reports NotEmpty.
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

    /// `rename`, with the same read-only-clearing retry on the source. NTFS moves read-only files
    /// fine, but an SMB server mapping unix modes may refuse.
    ///
    /// Windows-only, as in [`Self::remove_file_force`] — and this is the variant that hid the unix
    /// permission widening the hardest, because an earlier ambient version discarded the chmod's
    /// own failure with `let _ =`, so a rename could widen the source mode and still report the
    /// original error.
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
        use std::os::fd::AsRawFd;

        let source_name = nul_terminated(source_name, "source")?;
        let destination_name = nul_terminated(destination_name, "destination")?;
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
        use std::os::fd::AsRawFd;

        let source_name = nul_terminated(source_name, "source")?;
        let destination_name = nul_terminated(destination_name, "destination")?;
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
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOTSUP) {
            return Err(error);
        }
        self.rename_noreplace_by_claim(&source_name, destination_parent, &destination_name)
    }

    /// The no-replace rename for macOS volumes whose driver has no `renameatx_np` at all.
    ///
    /// Measured on macOS 27 (Darwin 27.0.0): the FSKit exFAT driver answers `ENOTSUP` for every
    /// `RENAME_EXCL` caller, so an exFAT root cannot take a lock lease or publish a single file
    /// through the primitive path. `O_CREAT|O_EXCL`, `mkdirat`, and `symlinkat` are implemented
    /// there and each fails when the name is taken, so claiming the destination first expresses
    /// the same exclusion: one writer wins the claim and only that winner performs the replacing
    /// rename it now owns. Publication stays atomic — the rename still swaps a complete entry into
    /// place — and the claim narrows the exclusion guarantee only against writers that bypass this
    /// protocol entirely, which `RENAME_EXCL` never covered either.
    #[cfg(target_os = "macos")]
    fn rename_noreplace_by_claim(
        &self,
        source_name: &std::ffi::CString,
        destination_parent: &Self,
        destination_name: &std::ffi::CString,
    ) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;

        let source_kind = {
            let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
            let result = unsafe {
                libc::fstatat(
                    self.handle.as_raw_fd(),
                    source_name.as_ptr(),
                    status.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error());
            }
            unsafe { status.assume_init() }.st_mode & libc::S_IFMT
        };

        let destination_fd = destination_parent.handle.as_raw_fd();
        // The claim has to be the source's own entry kind: POSIX `rename` replaces a directory
        // only with a directory, and a non-directory only with a non-directory.
        let claimed = match source_kind {
            libc::S_IFDIR => unsafe {
                libc::mkdirat(destination_fd, destination_name.as_ptr(), 0o700)
            },
            libc::S_IFLNK => unsafe {
                libc::symlinkat(
                    CLAIM_LINK_TARGET.as_ptr().cast(),
                    destination_fd,
                    destination_name.as_ptr(),
                )
            },
            _ => {
                let descriptor = unsafe {
                    libc::openat(
                        destination_fd,
                        destination_name.as_ptr(),
                        libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY,
                        libc::c_int::from(0o600),
                    )
                };
                if descriptor < 0 {
                    -1
                } else {
                    unsafe { libc::close(descriptor) }
                }
            }
        };
        if claimed != 0 {
            return Err(std::io::Error::last_os_error());
        }

        let renamed = unsafe {
            libc::renameat(
                self.handle.as_raw_fd(),
                source_name.as_ptr(),
                destination_fd,
                destination_name.as_ptr(),
            )
        };
        if renamed == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        // The claim reserves a name and holds no content. Leaving it behind would turn a failed
        // publication into an empty entry the next compare reports as a real difference.
        unsafe {
            libc::unlinkat(
                destination_fd,
                destination_name.as_ptr(),
                if source_kind == libc::S_IFDIR {
                    libc::AT_REMOVEDIR
                } else {
                    0
                },
            )
        };
        Err(error)
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

    /// Both Windows rename flavors submit `FILE_RENAME_INFORMATION` to `NtSetInformationFile`
    /// directly. The Win32 wrapper (`SetFileInformationByHandle`) accepts the same record only
    /// with `RootDirectory = NULL` and a full destination path; handing it a real directory
    /// handle fails with `ERROR_INVALID_PARAMETER` (measured on Win11 26200). Only the NT call
    /// performs the directory-relative form, which keeps the destination anchored to the held
    /// handle instead of a re-resolved path.
    ///
    /// Failures surface as the Win32 error `RtlNtStatusToDosError` assigns, so a taken
    /// destination under `replace_existing = false` reports `AlreadyExists`
    /// (`STATUS_OBJECT_NAME_COLLISION` → `ERROR_ALREADY_EXISTS`), which no-replace callers
    /// match on.
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
        use windows_sys::Wdk::Storage::FileSystem::{
            FileRenameInformation, NtSetInformationFile, FILE_RENAME_INFORMATION,
        };
        use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

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
        let allocation = std::mem::size_of::<FILE_RENAME_INFORMATION>()
            .checked_add(bytes.saturating_sub(std::mem::size_of::<u16>()))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "destination name is too long",
                )
            })?;
        let words = allocation.div_ceil(std::mem::size_of::<usize>());
        let mut storage = vec![0usize; words];
        let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        unsafe {
            (*information).Anonymous.ReplaceIfExists = replace_existing;
            (*information).RootDirectory = destination_parent.handle.as_raw_handle();
            (*information).FileNameLength = bytes as u32;
            std::ptr::copy_nonoverlapping(
                destination_name.as_ptr(),
                std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
                destination_name.len(),
            );
            let mut status_block: IO_STATUS_BLOCK = std::mem::zeroed();
            let status = NtSetInformationFile(
                source.as_raw_handle(),
                &mut status_block,
                information.cast(),
                allocation as u32,
                FileRenameInformation,
            );
            if status < 0 {
                Err(std::io::Error::from_raw_os_error(
                    RtlNtStatusToDosError(status) as i32,
                ))
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

    #[cfg(not(windows))]
    pub(super) fn sync_all(&self) -> std::io::Result<()> {
        self.handle.sync_all()
    }

    /// `FlushFileBuffers` demands a write-access handle, and the held directory handle is opened
    /// without write access — flushing through it fails with `ERROR_ACCESS_DENIED` (measured on
    /// Win11 26200). The flush therefore goes through a second handle produced by `NtOpenFile`
    /// with the held handle as `RootDirectory` and an empty relative name, which reopens the same
    /// directory object without consulting any path, so a concurrent rename of the tree cannot
    /// retarget the flush.
    #[cfg(windows)]
    pub(super) fn sync_all(&self) -> std::io::Result<()> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
        use windows_sys::Wdk::Storage::FileSystem::{
            NtOpenFile, FILE_OPEN_FOR_BACKUP_INTENT, FILE_SYNCHRONOUS_IO_NONALERT,
        };
        use windows_sys::Win32::Foundation::{RtlNtStatusToDosError, UNICODE_STRING};
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

        const GENERIC_WRITE: u32 = 0x4000_0000;
        const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
        const SHARE_READ: u32 = 0x0000_0001;
        const SHARE_WRITE: u32 = 0x0000_0002;
        const SHARE_DELETE: u32 = 0x0000_0004;

        let mut empty_buffer = [0u16; 1];
        let empty_name = UNICODE_STRING {
            Length: 0,
            MaximumLength: 0,
            Buffer: empty_buffer.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: self.handle.as_raw_handle(),
            ObjectName: &empty_name,
            Attributes: 0,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        let mut flush_handle = std::ptr::null_mut();
        let status = unsafe {
            NtOpenFile(
                &mut flush_handle,
                GENERIC_WRITE | SYNCHRONIZE_ACCESS,
                &attributes,
                &mut status_block,
                SHARE_READ | SHARE_WRITE | SHARE_DELETE,
                FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT,
            )
        };
        if status < 0 {
            return Err(std::io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32,
            ));
        }
        let writable = unsafe { std::fs::File::from_raw_handle(flush_handle) };
        writable.sync_all()
    }

    pub(super) fn identity(&self) -> std::io::Result<FileIdentity> {
        file_identity(&self.handle)
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
