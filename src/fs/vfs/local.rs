//! The local backend: std::fs + the existing `fs::staged::Staged`, nothing reinvented.
//!
//! `as_local()` returns the root, which routes scan down the existing
//! walkdir+mmap fast path — this impl is what the *generic* engine lanes use, and
//! its write side deliberately wraps the very same `Staged` the direct lane uses,
//! so local behavior cannot drift between lanes.
//!
//! Layering: this L0 module reaches up to L1 `obs::logging` through `log_warn!`, in `read_dir`,
//! where an entry whose name is not valid Unicode is skipped. The skip has no return channel —
//! the signature yields entries, not diagnostics — so without the log it is silent. See `lib.rs`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::model::table::EntryKind;
use super::error::VfsResult;
use super::{
    CaseSense, CommitReport, Medium, ReadStream, Support, VDirEntry, VMeta, Vfs, VfsCaps,
    WriteHint, WriteStaged,
};
use crate::foundation::path::join_native;
use crate::fs::staged::Staged;

pub struct LocalVfs {
    root: PathBuf,
    /// The root exactly as the job spelled it — `identity()` must reproduce it so
    /// existing hash-cache / mtime-fix files keep their names.
    root_str: String,
    /// Filled on first `caps()`, not on `connect()`: `apply` and the conformance harness build a
    /// `LocalVfs` and use it without connecting, and a capability sheet that depends on whether
    /// someone remembered to connect is a capability sheet that lies.
    vol: OnceLock<Volume>,
}

impl LocalVfs {
    pub fn new(root: PathBuf) -> LocalVfs {
        let root_str = root.to_string_lossy().into_owned();
        LocalVfs { root, root_str, vol: OnceLock::new() }
    }

    fn abs(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            join_native(&self.root, rel)
        }
    }

    /// What the OS says about the volume this root sits on. Probed once, then cached.
    pub fn volume(&self) -> &Volume {
        self.vol.get_or_init(|| probe(&self.root))
    }
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
        Volume { medium: Medium::Unknown, fs_name: String::new(), case_sensitivity: CaseSense::Unknown }
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
        | "btrfs" | "xfs" | "zfs" | "f2fs" | "jfs" | "reiserfs" | "tmpfs" | "overlay"
        | "msdos" | "vfat" | "exfat" | "fat" | "fat32" => Medium::FixedDisk,
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

/// Whether the central trash store can take this root's deletions.
///
/// Named rather than inlined because it is the whole point of probing the medium, and because
/// the failure it prevents is invisible in a diff: `true` on a share means every deleted file is
/// downloaded before being removed.
fn trash_reaches(medium: Medium) -> bool {
    matches!(medium, Medium::FixedDisk | Medium::RemovableDisk)
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
    let wide: Vec<u16> =
        std::ffi::OsStr::new(&vol_root).encode_wide().chain(std::iter::once(0)).collect();

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
        return Volume { medium, ..Volume::unknown() };
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

fn meta_of(md: &std::fs::Metadata) -> VMeta {
    let kind = if md.file_type().is_symlink() {
        EntryKind::Symlink
    } else if md.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    VMeta {
        kind,
        size: md.len(),
        mtime_ms: crate::foundation::time::meta_mtime_ms(md),
        mode: mode_of(md),
        file_id: file_id_of(md),
        link: None, // lazy: read_link on demand
    }
}

#[cfg(unix)]
fn mode_of(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(md.mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_md: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn file_id_of(md: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("{}:{}", md.dev(), md.ino()))
}

#[cfg(not(unix))]
fn file_id_of(_md: &std::fs::Metadata) -> Option<String> {
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
use std::io::Read as _;

/// `Staged` plus the write hint: mtime/mode land on the temp file (pre-rename, the
/// same order the direct apply lane uses), the post-rename stat feeds the
/// mtime-correction table.
struct LocalStaged {
    staged: Option<Staged>,
    dst: PathBuf,
    hint: WriteHint,
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
        s.seal(fsync)?;
        Ok(())
    }

    fn staged_len(&self) -> VfsResult<u64> {
        let s = self.staged.as_ref().expect("staged_len after commit");
        Ok(std::fs::metadata(s.path())?.len())
    }

    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>> {
        let s = self.staged.as_ref().expect("read after commit");
        Ok(Box::new(LocalRead { file: std::fs::File::open(s.path())? }))
    }

    fn local_path(&self) -> Option<&Path> {
        self.staged.as_ref().map(|s| s.path())
    }

    fn commit(mut self: Box<Self>) -> VfsResult<CommitReport> {
        let staged = self.staged.take().expect("double commit");
        let mut report = CommitReport::default();

        if let Some(ms) = self.hint.mtime_ms {
            let ft = filetime::FileTime::from_unix_time(ms.div_euclid(1000), (ms.rem_euclid(1000) * 1_000_000) as u32);
            if let Err(e) = filetime::set_file_mtime(staged.path(), ft) {
                report.mtime_error = Some(e.into());
            }
        }
        #[cfg(unix)]
        if let Some(mode) = self.hint.mode {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(mode)) {
                report.mode_error = Some(e.into());
            }
        }

        staged.commit()?;

        if self.hint.mtime_ms.is_some() {
            report.mtime_ondisk_ms = std::fs::metadata(&self.dst)
                .ok()
                .map(|md| crate::foundation::time::meta_mtime_ms(&md));
        }
        Ok(report)
    }
}

impl Vfs for LocalVfs {
    fn caps(&self) -> VfsCaps {
        let vol = self.volume();
        let networked = vol.medium == Medium::NetworkShare;
        VfsCaps {
            protocol: "local",
            // Measured, not assumed: FAT stores two-second mtimes and exFAT ten-millisecond ones,
            // and a root on either used to be described as if it were NTFS.
            mtime_precision_ms: mtime_precision_for(&vol.fs_name),
            set_mtime: Support::Yes,
            fsync: Support::Yes,
            rename: Support::Yes,
            rename_overwrite: Support::Yes,
            ranged_read: Support::Yes,
            write_at: Support::Yes,
            unix_mode: if cfg!(unix) { Support::Yes } else { Support::No },
            symlink: if cfg!(unix) { Support::Yes } else { Support::Unknown },
            file_id: if cfg!(unix) { Support::Yes } else { Support::No },
            free_space: Support::Yes,
            read_back: Support::Yes,
            medium: vol.medium,
            local_trash: trash_reaches(vol.medium),
            case_sensitivity: vol.case_sensitivity,
            // Whatever this process's own path layer enforces. SMB inherits this deliberately:
            // a Windows client gets Win32 name parsing even when the share is served by Samba.
            name_rules: super::NameRules::host(),
            // A share saturates its uplink long before sixteen streams; past that they only
            // queue against each other. FFS measured the same knee at two to four.
            max_parallel_streams: if networked { 4 } else { 16 },
        }
    }

    fn display(&self) -> String {
        self.root_str.clone()
    }

    fn identity(&self) -> String {
        self.root_str.clone()
    }

    fn as_local(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn connect(&self) -> VfsResult<()> {
        // Nothing to authenticate, but warm the volume probe so the capability sheet is settled
        // at a predictable moment rather than on whichever thread reads `caps()` first.
        let _ = self.volume();
        Ok(())
    }

    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        match std::fs::symlink_metadata(self.abs(rel)) {
            Ok(md) => Ok(Some(meta_of(&md))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.abs(rel))? {
            let entry = entry?;
            // A name that is not valid Unicode has no faithful rel: `to_string_lossy` would
            // substitute U+FFFD, and that spelling points at a different (nonexistent) file.
            // Skip it loudly rather than hand the engine a name it cannot act on. Scanning a
            // local or SMB root does not come through here (`as_local` sends it down the
            // walkdir lane, which counts these into the snapshot's walk errors) — this is the
            // directory-deletion probe, where a skipped entry makes remove_dir fail as
            // NotEmpty and the directory is honestly reported as kept.
            let Some(name) = entry.file_name().to_str().map(|s| s.to_owned()) else {
                crate::log_warn!(
                    "vfs",
                    "skipping '{}': name is not valid Unicode on this platform",
                    entry.path().to_string_lossy()
                );
                continue;
            };
            // lstat semantics, and a file vanishing between listing and stat is a scan race, not an error
            let md = match std::fs::symlink_metadata(entry.path()) {
                Ok(md) => md,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            out.push(VDirEntry { name, meta: meta_of(&md) });
        }
        Ok(out)
    }

    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        Ok(Box::new(LocalRead { file: std::fs::File::open(self.abs(rel))? }))
    }

    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        let mut f = std::fs::File::open(self.abs(rel))?;
        f.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; len as usize];
        let mut got = 0usize;
        while got < buf.len() {
            let n = f.read(&mut buf[got..])?;
            if n == 0 {
                break; // short only at EOF, per contract
            }
            got += n;
        }
        buf.truncate(got);
        Ok(buf)
    }

    fn read_link(&self, rel: &str) -> VfsResult<String> {
        Ok(std::fs::read_link(self.abs(rel))?.to_string_lossy().into_owned())
    }

    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        Ok(std::fs::create_dir_all(self.abs(rel))?)
    }

    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        let dst = self.abs(rel);
        let staged = Staged::create(&dst)?;
        Ok(Box::new(LocalStaged { staged: Some(staged), dst, hint: hint.clone() }))
    }

    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        // force: an SMB server mapping unix modes can refuse to move a read-only source
        Ok(crate::fs::rename_force(&self.abs(from_rel), &self.abs(to_rel))?)
    }

    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        // force: read-only files (git objects) must still be deletable, as on unix
        Ok(crate::fs::remove_file_force(&self.abs(rel))?)
    }

    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        // force: a directory carrying the Windows read-only attribute is refused by
        // RemoveDirectory just as a read-only file is refused by DeleteFile
        Ok(crate::fs::remove_dir_force(&self.abs(rel))?)
    }

    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        let ft = filetime::FileTime::from_unix_time(
            mtime_ms.div_euclid(1000),
            (mtime_ms.rem_euclid(1000) * 1_000_000) as u32,
        );
        Ok(filetime::set_file_mtime(self.abs(rel), ft)?)
    }

    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            Ok(std::fs::set_permissions(self.abs(rel), std::fs::Permissions::from_mode(mode))?)
        }
        #[cfg(not(unix))]
        {
            let _ = (rel, mode);
            Err(super::VfsError::new(super::VfsErrorKind::Unsupported, "unix modes do not exist on this filesystem"))
        }
    }

    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        #[cfg(unix)]
        {
            Ok(std::os::unix::fs::symlink(target, self.abs(rel))?)
        }
        #[cfg(windows)]
        {
            // File link first, directory link as the fallback (the target may be a dir).
            // Needs developer mode or a privilege; when both fail, the error says so
            // and preflight's Unknown-capability listing already warned.
            let dst = self.abs(rel);
            Ok(std::os::windows::fs::symlink_file(target, &dst)
                .or_else(|_| std::os::windows::fs::symlink_dir(target, &dst))?)
        }
    }

    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        Ok(crate::foundation::disk::disk_space(&self.root))
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
        assert_eq!(win_root_of(r"\\nas\photos\2026"), WinRoot::Share(r"\\nas\photos\".into()));
        assert_eq!(win_root_of(r"\\nas\photos"), WinRoot::Share(r"\\nas\photos\".into()));
        // The extended-length UNC spelling names the same share
        assert_eq!(win_root_of(r"\\?\UNC\nas\photos\sub"), WinRoot::Share(r"\\nas\photos\".into()));
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
        assert_eq!(win_root_of(r"\\?\D:\very\long"), WinRoot::Drive(r"D:\".into()));
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
        assert_eq!(mtime_precision_for(""), 1, "an unnamed filesystem is not assumed coarse");
    }

    #[test]
    fn network_filesystems_are_recognized_by_name() {
        for fs in ["smbfs", "cifs", "nfs", "afpfs", "sshfs", "SMB2"] {
            assert_eq!(medium_for_fs(fs), Medium::NetworkShare, "{fs}");
        }
        for fs in ["apfs", "ext4", "NTFS", "btrfs", "exfat"] {
            assert_eq!(medium_for_fs(fs), Medium::FixedDisk, "{fs}");
        }
        assert_eq!(medium_for_fs("fuse"), Medium::Unknown, "fuse could be either — say so");
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

    /// The bug this phase exists to fix. `true` on a share means `preserve` moves the original
    /// into the central store, the rename fails cross-volume, and the fallback copy pulls every
    /// deleted file across the network before deleting it.
    #[test]
    fn the_central_trash_is_claimed_only_for_volumes_on_this_machine() {
        assert!(trash_reaches(Medium::FixedDisk));
        assert!(trash_reaches(Medium::RemovableDisk), "a local copy is fast and costs no bandwidth");
        assert!(!trash_reaches(Medium::NetworkShare), "this is the download");
        assert!(!trash_reaches(Medium::Unknown), "not established is not the same as local");
    }

    #[test]
    fn a_real_local_root_probes_as_a_disk_on_this_machine() {
        let v = LocalVfs::new(std::env::temp_dir());
        let caps = v.caps();
        assert_ne!(caps.medium, Medium::NetworkShare, "the temp dir is not a share");
        assert!(caps.local_trash, "the temp dir's volume can take a trash move");
        // Probing twice must not re-probe or disagree with itself
        assert_eq!(v.volume(), v.volume());
    }
}
