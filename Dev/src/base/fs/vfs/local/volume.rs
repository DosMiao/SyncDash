//! What the OS reports about the volume under a root, and what that implies.
//!
//! Every entry here is a *probe result or a table keyed by filesystem name*, which is why they sit
//! together and apart from the backend that consumes them: they change when a filesystem or an OS
//! changes, not when the `Vfs` surface does. `Unknown` is always a real answer meaning "asked and
//! not told" — never a default standing in for a question nobody asked.

use std::path::Path;

use super::super::{CaseSense, Medium, Support};

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
    pub(super) fn unknown() -> Volume {
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
pub(super) fn mtime_precision_for(fs_name: &str) -> u32 {
    match fs_name.to_ascii_lowercase().as_str() {
        "fat" | "fat12" | "fat16" | "fat32" | "msdos" | "vfat" => 2000,
        "exfat" => 10,
        _ => 1,
    }
}

pub(super) fn fat_family(fs_name: &str) -> bool {
    matches!(
        fs_name.to_ascii_lowercase().as_str(),
        "fat" | "fat12" | "fat16" | "fat32" | "msdos" | "vfat" | "exfat"
    )
}

pub(super) fn file_ids_stable_for_fs(fs_name: &str) -> bool {
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

pub(super) fn unix_mode_support(fs_name: &str) -> Support {
    if cfg!(unix) && !fat_family(fs_name) {
        Support::Yes
    } else {
        Support::No
    }
}

pub(super) fn symlink_support_for_fs(fs_name: &str) -> Support {
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
pub(super) fn medium_for_fs(fs_name: &str) -> Medium {
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
pub(super) fn case_for_fs(fs_name: &str) -> CaseSense {
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
pub(super) fn central_trash_reaches(root: &Path, medium: Medium) -> bool {
    !matches!(medium, Medium::NetworkShare | Medium::Unknown)
        && crate::foundation::volume::same_device(root, &crate::foundation::dirs::data_dir())
}

pub(super) fn scan_streams(volume: &Volume) -> usize {
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

/// `GetDriveTypeW`'s return values. A CD-ROM counts as removable: what matters downstream is
/// only whether the central trash store is on the same volume, and it never is for either.
#[cfg(any(windows, test))]
pub(super) fn medium_for_win_drive_type(t: u32) -> Medium {
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
pub(super) fn probe(root: &Path) -> Volume {
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

    let (vol_root, shape) = match crate::foundation::volume::win_root_of(&root.to_string_lossy()) {
        crate::foundation::volume::WinRoot::Drive(d) => (d, Medium::Unknown),
        crate::foundation::volume::WinRoot::Share(s) => (s, Medium::NetworkShare),
        crate::foundation::volume::WinRoot::Unknown => return Volume::unknown(),
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
pub(super) fn probe(root: &Path) -> Volume {
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

/// The classification tables are pure on purpose: the mapping from "what the OS said" to "what
/// the engine may assume" is the part that has to be right, and it must be checkable without a FAT
/// stick, a NAS and a CD-ROM to hand.
#[cfg(test)]
mod volume_tests {
    use super::*;

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
        let same_volume =
            crate::foundation::volume::same_device(&root, &crate::foundation::dirs::data_dir());
        assert_eq!(central_trash_reaches(&root, Medium::FixedDisk), same_volume);
        assert_eq!(
            central_trash_reaches(&root, Medium::RemovableDisk),
            same_volume
        );
        assert!(!central_trash_reaches(&root, Medium::NetworkShare));
        assert!(!central_trash_reaches(&root, Medium::Unknown));
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
}
