//! The local backend, confined to one retained root directory handle.
//!
//! Every operation resolves segment-by-segment from that handle, so a path can never address
//! outside the root it was opened against. `read_dir` delegates to `LocalRoot::read_directory`,
//! which fails an entry whose name is not valid Unicode rather than skipping it: a name this
//! process cannot spell is a name it cannot later address, and a scan that silently omits one
//! would report a deletion the user never made.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::{ReadStream, Support, VDirEntry, VMeta, Vfs, VfsCaps, WriteHint, WriteStaged};
use crate::foundation::path::{RootRelativeDir, RootRelativePath};
use crate::fs::local_root::LocalRoot;

pub struct LocalVfs {
    root: LocalRoot,
    /// The root exactly as the job spelled it — `identity()` must reproduce it so
    /// existing hash-cache / mtime-fix files keep their names.
    root_str: String,
    /// Filled on first `caps()`, not on `connect()`: `apply` and the conformance harness build a
    /// `LocalVfs` and use it without connecting, and a capability sheet that depends on whether
    /// someone remembered to connect is a capability sheet that lies.
    vol: OnceLock<Volume>,
}

impl LocalVfs {
    pub fn open(root_path: PathBuf) -> VfsResult<LocalVfs> {
        let root_str = root_path.to_string_lossy().into_owned();
        Ok(LocalVfs {
            root: LocalRoot::open(root_path)?,
            root_str,
            vol: OnceLock::new(),
        })
    }

    pub fn local_root(&self) -> &LocalRoot {
        &self.root
    }

    /// What the OS says about the volume this root sits on. Probed once, then cached.
    pub fn volume(&self) -> &Volume {
        self.vol.get_or_init(|| probe(self.root.display_path()))
    }
}

fn relative_path(relative: &str) -> VfsResult<RootRelativePath> {
    RootRelativePath::try_from(relative).map_err(|error| {
        VfsError::new(
            VfsErrorKind::Protocol,
            format!("path is outside the local VFS contract: {error}"),
        )
    })
}

fn relative_directory(relative: &str) -> VfsResult<RootRelativeDir> {
    RootRelativeDir::try_from(relative).map_err(|error| {
        VfsError::new(
            VfsErrorKind::Protocol,
            format!("directory is outside the local VFS contract: {error}"),
        )
    })
}

mod meta;
mod staged;
pub mod volume;

pub use volume::Volume;

use self::meta::meta_of;
use self::staged::{LocalRead, LocalStaged};
use self::volume::{
    central_trash_reaches, file_ids_stable_for_fs, mtime_precision_for, probe, scan_streams,
    symlink_support_for_fs, unix_mode_support,
};

impl Vfs for LocalVfs {
    fn caps(&self) -> VfsCaps {
        let vol = self.volume();
        let symlink = symlink_support_for_fs(&vol.fs_name);
        VfsCaps {
            protocol: "local",
            // Measured, not assumed: FAT stores two-second mtimes and exFAT ten-millisecond ones,
            // and a root on either used to be described as if it were NTFS.
            mtime_precision_ms: mtime_precision_for(&vol.fs_name),
            set_mtime: Support::Yes,
            fsync: Support::Yes,
            rename: Support::Yes,
            rename_overwrite: Support::Yes,
            exclusive_staged_file_publish: if cfg!(any(
                target_os = "linux",
                target_os = "android",
                target_os = "macos",
                windows
            )) {
                Support::Yes
            } else {
                Support::No
            },
            exclusive_entry_rename: if cfg!(any(
                target_os = "linux",
                target_os = "android",
                target_os = "macos",
                windows
            )) {
                Support::Yes
            } else {
                Support::No
            },
            exclusive_symlink_publish: symlink,
            durable_namespace: if cfg!(unix) {
                Support::Yes
            } else {
                Support::Unknown
            },
            ranged_read: Support::Yes,
            write_at: Support::Yes,
            // Host syscalls are not the filesystem contract: FAT/exFAT synthesize permission
            // bits even when mounted on a Unix host.
            unix_mode: unix_mode_support(&vol.fs_name),
            // macOS FSKit-backed exFAT represents real links despite the portable exFAT format
            // having no POSIX mode bits. Other FAT drivers cannot be assumed to do so.
            symlink,
            file_id: if cfg!(unix) && file_ids_stable_for_fs(&vol.fs_name) {
                Support::Yes
            } else {
                Support::No
            },
            free_space: Support::Yes,
            read_back: Support::Yes,
            medium: vol.medium,
            local_trash: central_trash_reaches(self.root.display_path(), vol.medium),
            case_sensitivity: vol.case_sensitivity,
            // Whatever this process's own path layer enforces. SMB inherits this deliberately:
            // a Windows client gets Win32 name parsing even when the share is served by Samba.
            name_rules: super::NameRules::host(),
            // A share saturates its uplink long before sixteen streams; past that they only
            // queue against each other. FFS measured the same knee at two to four.
            max_parallel_streams: scan_streams(vol),
        }
    }

    fn display(&self) -> String {
        self.root_str.clone()
    }

    fn identity(&self) -> String {
        self.root_str.clone()
    }

    fn local_root(&self) -> Option<&LocalRoot> {
        Some(&self.root)
    }

    fn connect(&self) -> VfsResult<()> {
        // Nothing to authenticate, but warm the volume probe so the capability sheet is settled
        // at a predictable moment rather than on whichever thread reads `caps()` first.
        let _ = self.volume();
        Ok(())
    }

    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        let metadata = if rel.is_empty() {
            self.root.metadata_directory(&relative_directory(rel)?)
        } else {
            self.root.metadata_path(&relative_path(rel)?)
        };
        match metadata {
            Ok(metadata) => Ok(Some(meta_of(&metadata))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        Ok(self
            .root
            .read_directory(&relative_directory(rel)?)?
            .into_iter()
            .map(|entry| VDirEntry {
                name: entry.name,
                meta: meta_of(&entry.metadata),
            })
            .collect())
    }

    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        Ok(Box::new(LocalRead::new(
            self.root.open_read(&relative_path(rel)?)?,
        )))
    }

    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        Ok(self
            .root
            .read_range(&relative_path(rel)?, off, len as usize)?)
    }

    fn read_link(&self, rel: &str) -> VfsResult<String> {
        Ok(self
            .root
            .read_link(&relative_path(rel)?)?
            .to_string_lossy()
            .into_owned())
    }

    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        Ok(self.root.create_directory_all(&relative_directory(rel)?)?)
    }

    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        let destination = relative_path(rel)?;
        let staged = self.root.create_staged(&destination)?;
        Ok(Box::new(LocalStaged::new(
            self.root.clone(),
            staged,
            destination,
            hint.clone(),
        )))
    }

    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        Ok(self
            .root
            .rename(&relative_path(from_rel)?, &relative_path(to_rel)?)?)
    }

    fn rename_noreplace(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        Ok(self
            .root
            .rename_noreplace(&relative_path(from_rel)?, &relative_path(to_rel)?)?)
    }

    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        Ok(self.root.remove_file(&relative_path(rel)?)?)
    }

    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        Ok(self.root.remove_directory(&relative_directory(rel)?)?)
    }

    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        Ok(self.root.set_mtime(&relative_path(rel)?, mtime_ms)?)
    }

    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        #[cfg(unix)]
        {
            Ok(self.root.set_mode(&relative_path(rel)?, mode)?)
        }
        #[cfg(not(unix))]
        {
            let _ = (rel, mode);
            Err(super::VfsError::new(
                super::VfsErrorKind::Unsupported,
                "unix modes do not exist on this filesystem",
            ))
        }
    }

    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        Ok(self
            .root
            .make_symlink(&relative_path(rel)?, Path::new(target))?)
    }

    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        Ok(crate::foundation::disk::disk_space(
            self.root.display_path(),
        ))
    }
}

/// The backend's own behavior, as distinct from the classification tables it consults.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::Medium;

    #[test]
    fn a_real_local_root_probes_as_a_disk_on_this_machine() {
        let v = LocalVfs::open(std::env::temp_dir()).unwrap();
        let caps = v.caps();
        assert_ne!(
            caps.medium,
            Medium::NetworkShare,
            "the temp dir is not a share"
        );
        assert_eq!(
            caps.local_trash,
            crate::foundation::volume::same_device(
                &std::env::temp_dir(),
                &crate::foundation::dirs::data_dir(),
            )
        );
        // Probing twice must not re-probe or disagree with itself
        assert_eq!(v.volume(), v.volume());
    }
}
