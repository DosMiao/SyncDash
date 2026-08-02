//! The local backend, confined to one retained root directory handle.
//!
//! Layering: this L0 module reaches up to L1 `obs::logging` through `log_warn!`, in `read_dir`,
//! where an entry whose name is not valid Unicode is skipped. The skip has no return channel —
//! the signature yields entries, not diagnostics — so without the log it is silent. See `lib.rs`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::VfsEntryKind;
use super::{
    CaseSense, CommitReport, Medium, ReadStream, Support, VDirEntry, VMeta, Vfs, VfsCaps,
    WriteHint, WriteStaged,
};
use crate::foundation::path::{RootRelativeDir, RootRelativePath};
use crate::fs::local_root::{LocalRoot, LocalStagedFile};

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

/// What the OS reported about a volume. `Unknown` fields mean the question was asked and not
/// answered — never that it was skipped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub medium: Medium,
    /// Filesystem name as the OS spells it (`NTFS`, `exFAT`, `apfs`, `smbfs`). Empty = unavailable.
    pub fs_name: String,
    pub case_sensitivity: CaseSense,
}

impl Volume {
    fn unknown() -> Volume {
        Volume {
            medium: Medium::Unknown,
            fs_name: String::new(),
            case_sensitivity: CaseSense::Unknown,
        }
    }
}

/// mtime granularity a filesystem actually stores, in ms.
///
/// FAT keeps two-second resolution for mtime and exFAT keeps 10 ms; everything else here is
/// sub-millisecond. This is what lets the compare window stop being a blanket 2 s for every root.
fn mtime_precision_for(fs_name: &str) -> u32 {
    match fs_name.to_ascii_lowercase().as_str() {
        "fat" | "fat12" | "fat16" | "fat32" | "msdos" | "vfat" => 2000,
        "exfat" => 10,
        _ => 1,
    }
}

fn fat_family(fs_name: &str) -> bool {
    matches!(
        fs_name.to_ascii_lowercase().as_str(),
        "fat" | "fat12" | "fat16" | "fat32" | "msdos" | "vfat" | "exfat"
    )
}

fn file_ids_stable_for_fs(fs_name: &str) -> bool {
    matches!(
        fs_name.to_ascii_lowercase().as_str(),
        "apfs"
            | "hfs"
            | "hfsplus"
            | "ext2"
            | "ext3"
            | "ext4"
            | "btrfs"
            | "xfs"
            | "zfs"
            | "f2fs"
            | "jfs"
            | "ntfs"
            | "ntfs3"
            | "refs"
    )
}

fn unix_mode_support(fs_name: &str) -> Support {
    if cfg!(unix) && !fat_family(fs_name) {
        Support::Yes
    } else {
        Support::No
    }
}

fn symlink_support_for_fs(fs_name: &str) -> Support {
    if cfg!(target_os = "macos") && fs_name.eq_ignore_ascii_case("exfat") {
        Support::Yes
    } else if fat_family(fs_name) {
        Support::No
    } else if cfg!(unix) {
        Support::Yes
    } else {
        Support::Unknown
    }
}

/// Classify a filesystem by the name the OS gives it.
///
/// Used by the unix probe, where `statfs` reports a name but no medium. Windows does not need it:
/// `GetDriveTypeW` answers the same question directly, and for a mount there is no name to read.
#[cfg(any(unix, test))]
fn medium_for_fs(fs_name: &str) -> Medium {
    match fs_name.to_ascii_lowercase().as_str() {
        "smbfs" | "cifs" | "smb2" | "smb3" | "nfs" | "nfs4" | "afpfs" | "webdav" | "ftp"
        | "sshfs" | "davfs" | "9p" => Medium::NetworkShare,
        "ntfs" | "ntfs3" | "refs" | "apfs" | "hfs" | "hfsplus" | "ext2" | "ext3" | "ext4"
        | "btrfs" | "xfs" | "zfs" | "f2fs" | "jfs" | "reiserfs" | "tmpfs" | "overlay" | "msdos"
        | "vfat" | "exfat" | "fat" | "fat32" => Medium::FixedDisk,
        _ => Medium::Unknown,
    }
}

/// Case sensitivity implied by a filesystem name, where the name settles it.
///
/// APFS and HFS+ are deliberately absent: both ship case-insensitive by default and can be
/// formatted either way, so the name does not answer the question and `Unknown` is the truth.
/// Windows reads the real flag off the volume instead and never consults this.
#[cfg(any(unix, test))]
fn case_for_fs(fs_name: &str) -> CaseSense {
    match fs_name.to_ascii_lowercase().as_str() {
        "ext2" | "ext3" | "ext4" | "btrfs" | "xfs" | "zfs" | "f2fs" | "jfs" | "reiserfs"
        | "tmpfs" | "overlay" => CaseSense::Sensitive,
        "msdos" | "vfat" | "exfat" | "fat" | "fat32" | "refs" => CaseSense::Insensitive,
        _ => CaseSense::Unknown,
    }
}

/// Whether the default central trash can receive this root through a same-volume rename.
///
/// "Local disk" is not sufficient: an external SSD and the user's cache directory are both local
/// but still different volumes. Falling back from that rename to a copy makes every deletion read
/// the complete old file off the removable disk. In-root retention is both safer and faster there.
fn central_trash_reaches(root: &Path, medium: Medium) -> bool {
    !matches!(medium, Medium::NetworkShare | Medium::Unknown)
        && same_device(root, &crate::foundation::dirs::data_dir())
}

fn scan_streams(volume: &Volume) -> usize {
    let base = if volume.medium == Medium::NetworkShare {
        4
    } else {
        match volume.fs_name.to_ascii_lowercase().as_str() {
            // FAT/exFAT trees are commonly removable media with many small files. Four readers
            // under-fill a modern external SSD, while the old fixed width of sixteen creates a
            // deep random-I/O queue on spinning media. Half the available CPUs, bounded to 4..=8,
            // is the same conservative shape used by mature parallel hashers: enough overlap for
            // metadata/open latency without turning every directory into an I/O storm.
            "fat" | "fat12" | "fat16" | "fat32" | "msdos" | "vfat" | "exfat" => {
                std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(4)
                    .div_ceil(2)
                    .clamp(4, 8)
            }
            _ => 16,
        }
    };
    crate::foundation::thread::configured_worker_limit().map_or(base, |limit| base.min(limit))
}

#[cfg(unix)]
pub fn same_device(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    fn device(path: &Path) -> Option<u64> {
        let mut candidate = Some(path);
        while let Some(path) = candidate {
            match std::fs::metadata(path) {
                Ok(metadata) => return Some(metadata.dev()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    candidate = path.parent();
                }
                Err(_) => return None,
            }
        }
        None
    }

    matches!((device(left), device(right)), (Some(left), Some(right)) if left == right)
}

#[cfg(windows)]
pub fn same_device(left: &Path, right: &Path) -> bool {
    match (
        win_root_of(&left.to_string_lossy()),
        win_root_of(&right.to_string_lossy()),
    ) {
        (WinRoot::Drive(left), WinRoot::Drive(right))
        | (WinRoot::Share(left), WinRoot::Share(right)) => left.eq_ignore_ascii_case(&right),
        _ => false,
    }
}

/// The volume root Windows' volume APIs want, derived from a root path.
///
/// `GetDriveTypeW` and `GetVolumeInformationW` both take a volume root with a trailing backslash
/// and nothing deeper. Kept pure so the spellings — UNC, extended-length, bare drive — are
/// testable without those volumes existing.
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
enum WinRoot {
    /// `D:\` — the drive type has to be asked for.
    Drive(String),
    /// `\\host\share\` — reached over the network by construction, no call needed.
    Share(String),
    Unknown,
}

#[cfg(any(windows, test))]
fn win_root_of(path: &str) -> WinRoot {
    let s = path.replace('/', "\\");
    // Extended-length prefixes bypass Win32 path parsing but name the same volumes.
    let s = if let Some(r) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{r}")
    } else if let Some(r) = s.strip_prefix(r"\\?\") {
        r.to_string()
    } else {
        s
    };
    if let Some(rest) = s.strip_prefix(r"\\") {
        let mut seg = rest.splitn(3, '\\');
        let (host, share) = (seg.next().unwrap_or(""), seg.next().unwrap_or(""));
        if host.is_empty() || share.is_empty() {
            return WinRoot::Unknown;
        }
        return WinRoot::Share(format!(r"\\{host}\{share}\"));
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return WinRoot::Drive(format!("{}:\\", (b[0] as char).to_ascii_uppercase()));
    }
    WinRoot::Unknown
}

/// `GetDriveTypeW`'s return values. A CD-ROM counts as removable: what matters downstream is
/// only whether the central trash store is on the same volume, and it never is for either.
#[cfg(any(windows, test))]
fn medium_for_win_drive_type(t: u32) -> Medium {
    match t {
        2 => Medium::RemovableDisk, // DRIVE_REMOVABLE
        3 => Medium::FixedDisk,     // DRIVE_FIXED
        4 => Medium::NetworkShare,  // DRIVE_REMOTE
        5 => Medium::RemovableDisk, // DRIVE_CDROM
        6 => Medium::FixedDisk,     // DRIVE_RAMDISK
        _ => Medium::Unknown,       // DRIVE_UNKNOWN / DRIVE_NO_ROOT_DIR
    }
}

#[cfg(windows)]
fn probe(root: &Path) -> Volume {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(lp_root_path_name: *const u16) -> u32;
        fn GetVolumeInformationW(
            lp_root_path_name: *const u16,
            lp_volume_name_buffer: *mut u16,
            n_volume_name_size: u32,
            lp_volume_serial_number: *mut u32,
            lp_maximum_component_length: *mut u32,
            lp_file_system_flags: *mut u32,
            lp_file_system_name_buffer: *mut u16,
            n_file_system_name_size: u32,
        ) -> i32;
    }
    const FILE_CASE_SENSITIVE_SEARCH: u32 = 0x0000_0001;

    let (vol_root, shape) = match win_root_of(&root.to_string_lossy()) {
        WinRoot::Drive(d) => (d, Medium::Unknown),
        WinRoot::Share(s) => (s, Medium::NetworkShare),
        WinRoot::Unknown => return Volume::unknown(),
    };
    let wide: Vec<u16> = std::ffi::OsStr::new(&vol_root)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // A UNC path is already settled; only a drive letter needs the call.
    let medium = if shape == Medium::NetworkShare {
        shape
    } else {
        medium_for_win_drive_type(unsafe { GetDriveTypeW(wide.as_ptr()) })
    };

    let mut fs_buf = [0u16; 64];
    let mut flags: u32 = 0;
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        )
    };
    if ok == 0 {
        // The medium is still worth keeping — an offline share answers the drive type fine.
        return Volume {
            medium,
            ..Volume::unknown()
        };
    }
    let n = fs_buf.iter().position(|&c| c == 0).unwrap_or(fs_buf.len());
    Volume {
        medium,
        fs_name: String::from_utf16_lossy(&fs_buf[..n]),
        // On Win32 the flag being clear is a real answer, not a missing one: the path layer
        // folds case for every write leaving this machine, whatever the server does.
        case_sensitivity: if flags & FILE_CASE_SENSITIVE_SEARCH != 0 {
            CaseSense::Sensitive
        } else {
            CaseSense::Insensitive
        },
    }
}

#[cfg(unix)]
fn probe(root: &Path) -> Volume {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(root.as_os_str().as_bytes()) else {
        return Volume::unknown();
    };
    let fs_name = unsafe {
        let mut st: libc::statfs = std::mem::zeroed();
        if libc::statfs(c.as_ptr(), &mut st) != 0 {
            return Volume::unknown();
        }
        fs_name_of(&st)
    };
    Volume {
        medium: medium_for_fs(&fs_name),
        case_sensitivity: case_for_fs(&fs_name),
        fs_name,
    }
}

/// BSD-family `statfs` carries the filesystem name outright.
#[cfg(all(unix, not(target_os = "linux")))]
unsafe fn fs_name_of(st: &libc::statfs) -> String {
    let raw = &st.f_fstypename;
    let n = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    raw[..n].iter().map(|&c| c as u8 as char).collect()
}

/// Linux `statfs` carries a magic number instead of a name, so the name is reconstructed.
/// FUSE is deliberately absent: it could be sshfs or it could be a local disk, and guessing
/// either way would be worse than reporting `Unknown`.
#[cfg(target_os = "linux")]
unsafe fn fs_name_of(st: &libc::statfs) -> String {
    let name = match st.f_type as i64 {
        0xFF53_4D42 | 0xFE53_4D42 | 0x517B => "cifs",
        0x6969 => "nfs",
        0x5346_414F => "afpfs",
        0x564C => "ncpfs",
        0xEF53 => "ext4",
        0x9123_683E => "btrfs",
        0x5846_5342 => "xfs",
        0x2FC1_2FC1 => "zfs",
        0xF2F5_2010 => "f2fs",
        0x0102_1994 => "tmpfs",
        0x794C_7630 => "overlay",
        0x5346_544E => "ntfs",
        0x4D44 => "vfat",
        0x2011_BAB0 => "exfat",
        _ => "",
    };
    name.to_string()
}

fn meta_of(metadata: &cap_primitives::fs::Metadata) -> VMeta {
    let kind = if metadata.is_symlink() {
        VfsEntryKind::Symlink
    } else if metadata.is_dir() {
        VfsEntryKind::Directory
    } else {
        VfsEntryKind::File
    };
    VMeta {
        kind,
        size: metadata.len(),
        mtime_ms: metadata_mtime_ms(metadata),
        mode: mode_of(metadata),
        file_id: file_id_of(metadata),
        link: None,
    }
}

fn metadata_mtime_ms(metadata: &cap_primitives::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .map(|time| crate::foundation::time::systime_ms(time.into_std()))
        .unwrap_or(0)
}

#[cfg(unix)]
fn mode_of(metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    use cap_primitives::fs::MetadataExt;
    Some(metadata.mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn file_id_of(metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    use cap_primitives::fs::MetadataExt;
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_id_of(_metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    None
}

struct LocalRead {
    file: std::fs::File,
}

impl std::io::Read for LocalRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl ReadStream for LocalRead {
    fn block_size(&self) -> usize {
        1024 * 1024
    }
}
struct LocalStaged {
    root: LocalRoot,
    staged: Option<LocalStagedFile>,
    destination: RootRelativePath,
    hint: WriteHint,
    sync_requested: bool,
}

impl LocalStaged {
    fn commit_with(&mut self, replace: bool) -> VfsResult<CommitReport> {
        let staged = self.staged.take().expect("double commit");
        let mut report = CommitReport::default();

        if let Some(ms) = self.hint.mtime_ms {
            if let Err(error) = staged.set_mtime(ms) {
                report.mtime_error = Some(error.into());
            }
        }
        #[cfg(unix)]
        if let Some(mode) = self.hint.mode {
            if let Err(error) = staged.set_mode(mode) {
                report.mode_error = Some(error.into());
            }
        }
        if self.sync_requested
            && (self.hint.mtime_ms.is_some() || (cfg!(unix) && self.hint.mode.is_some()))
        {
            staged.sync_file()?;
        }

        if replace {
            staged.commit()?;
        } else {
            staged.commit_noreplace()?;
        }

        if self.hint.mtime_ms.is_some() {
            report.mtime_ondisk_ms = self
                .root
                .metadata_path(&self.destination)
                .ok()
                .map(|metadata| metadata_mtime_ms(&metadata));
        }
        Ok(report)
    }
}

impl WriteStaged for LocalStaged {
    fn write(&mut self, buf: &[u8]) -> VfsResult<()> {
        let s = self.staged.as_mut().expect("write after commit");
        s.write_all_from(&mut &buf[..])?;
        Ok(())
    }

    fn block_size(&self) -> usize {
        1024 * 1024
    }

    fn write_at(&mut self, off: u64, buf: &[u8]) -> VfsResult<()> {
        let s = self.staged.as_mut().expect("write after commit");
        s.write_at(off, buf)?;
        Ok(())
    }

    fn seal(&mut self, fsync: bool) -> VfsResult<()> {
        let s = self.staged.as_mut().expect("seal after commit");
        self.sync_requested |= fsync;
        s.seal(fsync)?;
        Ok(())
    }

    fn staged_len(&self) -> VfsResult<u64> {
        let s = self.staged.as_ref().expect("staged_len after commit");
        Ok(s.staged_len()?)
    }

    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>> {
        let s = self.staged.as_ref().expect("read after commit");
        Ok(Box::new(LocalRead {
            file: s.try_clone_file()?,
        }))
    }

    fn commit(mut self: Box<Self>) -> VfsResult<CommitReport> {
        self.commit_with(true)
    }

    fn commit_noreplace(mut self: Box<Self>) -> VfsResult<CommitReport> {
        self.commit_with(false)
    }
}

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
        Ok(Box::new(LocalRead {
            file: self.root.open_read(&relative_path(rel)?)?,
        }))
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
        Ok(Box::new(LocalStaged {
            root: self.root.clone(),
            staged: Some(staged),
            destination,
            hint: hint.clone(),
            sync_requested: false,
        }))
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

/// The volume probe's classification tables. They are pure on purpose: the mapping from "what
/// the OS said" to "what the engine may assume" is the part that has to be right, and it must be
/// checkable without a FAT stick, a NAS and a CD-ROM to hand.
#[cfg(test)]
mod volume_tests {
    use super::*;

    #[test]
    fn a_unc_root_is_a_share_whatever_the_spelling() {
        assert_eq!(
            win_root_of(r"\\nas\photos\2026"),
            WinRoot::Share(r"\\nas\photos\".into())
        );
        assert_eq!(
            win_root_of(r"\\nas\photos"),
            WinRoot::Share(r"\\nas\photos\".into())
        );
        // The extended-length UNC spelling names the same share
        assert_eq!(
            win_root_of(r"\\?\UNC\nas\photos\sub"),
            WinRoot::Share(r"\\nas\photos\".into())
        );
        // A host with no share names no volume — better Unknown than a wrong guess
        assert_eq!(win_root_of(r"\\nas"), WinRoot::Unknown);
    }

    #[test]
    fn a_drive_root_survives_every_spelling() {
        assert_eq!(win_root_of(r"D:\Code\x"), WinRoot::Drive(r"D:\".into()));
        assert_eq!(win_root_of("D:/Code/x"), WinRoot::Drive(r"D:\".into()));
        assert_eq!(win_root_of("d:/code"), WinRoot::Drive(r"D:\".into()));
        assert_eq!(win_root_of("D:"), WinRoot::Drive(r"D:\".into()));
        // \\?\ is a parsing escape, not a different volume
        assert_eq!(
            win_root_of(r"\\?\D:\very\long"),
            WinRoot::Drive(r"D:\".into())
        );
        // A relative root names no volume
        assert_eq!(win_root_of("relative/dir"), WinRoot::Unknown);
    }

    #[test]
    fn drive_types_map_to_media() {
        assert_eq!(medium_for_win_drive_type(3), Medium::FixedDisk);
        assert_eq!(medium_for_win_drive_type(4), Medium::NetworkShare);
        assert_eq!(medium_for_win_drive_type(2), Medium::RemovableDisk);
        assert_eq!(medium_for_win_drive_type(5), Medium::RemovableDisk);
        // DRIVE_UNKNOWN and DRIVE_NO_ROOT_DIR are not "local"
        assert_eq!(medium_for_win_drive_type(0), Medium::Unknown);
        assert_eq!(medium_for_win_drive_type(1), Medium::Unknown);
    }

    #[test]
    fn fat_and_exfat_keep_their_real_mtime_granularity() {
        // The reason this exists: a 2 s window applied to every root hid real edits on NTFS,
        // and a 1 ms window applied to FAT reported drift that was only rounding.
        assert_eq!(mtime_precision_for("FAT32"), 2000);
        assert_eq!(mtime_precision_for("msdos"), 2000);
        assert_eq!(mtime_precision_for("exFAT"), 10);
        assert_eq!(mtime_precision_for("NTFS"), 1);
        assert_eq!(mtime_precision_for("apfs"), 1);
        assert_eq!(
            mtime_precision_for(""),
            1,
            "an unnamed filesystem is not assumed coarse"
        );
    }

    #[test]
    fn fat_and_exfat_do_not_claim_unix_modes() {
        for fs in ["FAT32", "msdos", "vfat", "exFAT"] {
            assert_eq!(unix_mode_support(fs), Support::No, "{fs}");
        }

        if cfg!(unix) {
            assert_eq!(unix_mode_support("apfs"), Support::Yes);
        }
    }

    #[test]
    fn file_id_capability_is_positive_listed() {
        for fs in ["apfs", "ext4", "btrfs", "xfs", "NTFS"] {
            assert!(file_ids_stable_for_fs(fs), "{fs}");
        }
        for fs in ["exFAT", "overlay", "tmpfs", "fuse", "mysteryfs", ""] {
            assert!(!file_ids_stable_for_fs(fs), "{fs}");
        }
    }

    #[test]
    fn symlink_support_accounts_for_the_filesystem_driver() {
        let exfat = if cfg!(target_os = "macos") {
            Support::Yes
        } else {
            Support::No
        };
        assert_eq!(symlink_support_for_fs("exFAT"), exfat);
        assert_eq!(symlink_support_for_fs("FAT32"), Support::No);

        if cfg!(unix) {
            assert_eq!(symlink_support_for_fs("apfs"), Support::Yes);
        }
    }

    #[test]
    fn network_filesystems_are_recognized_by_name() {
        for fs in ["smbfs", "cifs", "nfs", "afpfs", "sshfs", "SMB2"] {
            assert_eq!(medium_for_fs(fs), Medium::NetworkShare, "{fs}");
        }
        for fs in ["apfs", "ext4", "NTFS", "btrfs", "exfat"] {
            assert_eq!(medium_for_fs(fs), Medium::FixedDisk, "{fs}");
        }
        assert_eq!(
            medium_for_fs("fuse"),
            Medium::Unknown,
            "fuse could be either — say so"
        );
        assert_eq!(medium_for_fs(""), Medium::Unknown);
    }

    #[test]
    fn case_sensitivity_is_claimed_only_where_the_name_settles_it() {
        assert_eq!(case_for_fs("ext4"), CaseSense::Sensitive);
        assert_eq!(case_for_fs("exfat"), CaseSense::Insensitive);
        // Both ship case-insensitive by default and can be formatted either way
        assert_eq!(case_for_fs("apfs"), CaseSense::Unknown);
        assert_eq!(case_for_fs("hfs"), CaseSense::Unknown);
        assert_eq!(case_for_fs("smbfs"), CaseSense::Unknown);
    }

    /// Central retention is a rename only when the root and state directory share a volume.
    #[test]
    fn the_central_trash_is_claimed_only_for_the_same_physical_volume() {
        let root = std::env::temp_dir();
        let same_volume = same_device(&root, &crate::foundation::dirs::data_dir());
        assert_eq!(central_trash_reaches(&root, Medium::FixedDisk), same_volume);
        assert_eq!(
            central_trash_reaches(&root, Medium::RemovableDisk),
            same_volume
        );
        assert!(!central_trash_reaches(&root, Medium::NetworkShare));
        assert!(!central_trash_reaches(&root, Medium::Unknown));
    }

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
            same_device(&std::env::temp_dir(), &crate::foundation::dirs::data_dir())
        );
        // Probing twice must not re-probe or disagree with itself
        assert_eq!(v.volume(), v.volume());
    }

    #[test]
    fn small_file_filesystems_use_a_bounded_scan_width() {
        let exfat = Volume {
            medium: Medium::FixedDisk,
            fs_name: "exfat".into(),
            case_sensitivity: CaseSense::Insensitive,
        };
        let network = Volume {
            medium: Medium::NetworkShare,
            fs_name: "smbfs".into(),
            case_sensitivity: CaseSense::Unknown,
        };
        assert!((1..=8).contains(&scan_streams(&exfat)));
        assert!((1..=4).contains(&scan_streams(&network)));
        assert!(!file_ids_stable_for_fs(&exfat.fs_name));
    }

    #[test]
    fn nested_roots_are_recognized_as_the_same_device() {
        let root = std::env::temp_dir().join(format!("syncdash-device-{}", std::process::id()));
        let nested = root.join("nested");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&nested).unwrap();
        assert!(same_device(&root, &nested));
        let _ = std::fs::remove_dir_all(&root);
    }
}
