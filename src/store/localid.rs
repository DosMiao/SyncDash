//! Physical identity for machine-local scan state.
//!
//! `LocalVfs::identity()` intentionally remains the root exactly as the job spelled it because
//! changing that string would orphan every historical cache filename. Persistent state needs a
//! stronger answer inside that file, though: `/Volumes/Backup/Code` can name a different disk
//! after an unplug/replug. This helper binds a cache generation to both the mounted volume and the
//! canonical root within it, without changing the historical filename key.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalScanStateIdentity {
    cache_key: String,
    binding: Vec<u8>,
    file_ids_stable: bool,
}

impl LocalScanStateIdentity {
    pub(crate) fn for_root(root: &Path) -> LocalScanStateIdentity {
        let canonical = canonical_or_absolute(root);
        let platform = platform_identity(&canonical);
        LocalScanStateIdentity {
            // The slot stays pinned to the pre-versioning formula. Headerless local JSONL remains
            // discoverable there, but is rejected and replaced after a successful bound scan.
            cache_key: root.to_string_lossy().into_owned(),
            binding: encode_binding(&platform.volume, &platform.relative_root),
            file_ids_stable: platform.file_ids_stable,
        }
    }

    pub(crate) fn cache_key(&self) -> &str {
        &self.cache_key
    }

    pub(crate) fn binding(&self) -> &[u8] {
        &self.binding
    }

    /// FAT-family object identifiers are allocation artifacts, not durable rename evidence. They
    /// may be reused after deletion and can change across a remount, so callers must omit them
    /// from snapshots on those filesystems.
    pub(crate) fn file_ids_stable(&self) -> bool {
        self.file_ids_stable
    }
}

#[derive(Debug)]
struct PlatformIdentity {
    volume: Vec<u8>,
    relative_root: Vec<u8>,
    file_ids_stable: bool,
}

fn encode_binding(volume: &[u8], relative_root: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + volume.len() + relative_root.len());
    out.extend_from_slice(b"syncdash.local-scan-state\0");
    out.extend_from_slice(&(volume.len() as u64).to_le_bytes());
    out.extend_from_slice(volume);
    out.extend_from_slice(&(relative_root.len() as u64).to_le_bytes());
    out.extend_from_slice(relative_root);
    out
}

fn canonical_or_absolute(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| {
        if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir().map_or_else(|_| root.to_path_buf(), |cwd| cwd.join(root))
        }
    })
}

fn fat_family(fs_name: &str) -> bool {
    matches!(
        fs_name.to_ascii_lowercase().as_str(),
        "fat" | "fat12" | "fat16" | "fat32" | "msdos" | "vfat" | "exfat"
    )
}

#[cfg(target_os = "macos")]
fn platform_identity(canonical: &Path) -> PlatformIdentity {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let dev = std::fs::metadata(canonical).ok().map(|metadata| metadata.dev());
    let (mount_root, fs_name) = macos_mount(canonical)
        .unwrap_or_else(|| (device_root(canonical, dev), String::new()));
    let volume = macos_volume_uuid(&mount_root)
        .map(|uuid| format_uuid("macos-uuid", &uuid).into_bytes())
        .or_else(|| dev.map(|value| format!("macos-dev:{value}").into_bytes()))
        .unwrap_or_else(|| b"macos-volume:unknown".to_vec());
    let relative = canonical.strip_prefix(&mount_root).unwrap_or(canonical);
    let relative_root = relative.as_os_str().as_bytes().to_vec();

    PlatformIdentity {
        volume,
        relative_root,
        file_ids_stable: !fat_family(&fs_name),
    }
}

#[cfg(target_os = "macos")]
fn macos_mount(path: &Path) -> Option<(PathBuf, String)> {
    use std::ffi::{CStr, CString, OsString};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
        return None;
    }
    let mount = unsafe { CStr::from_ptr(info.f_mntonname.as_ptr()) }.to_bytes();
    let fs_name = unsafe { CStr::from_ptr(info.f_fstypename.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Some((PathBuf::from(OsString::from_vec(mount.to_vec())), fs_name))
}

#[cfg(target_os = "macos")]
fn macos_volume_uuid(mount_root: &Path) -> Option<[u8; 16]> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct UuidBuffer {
        length: u32,
        uuid: [u8; 16],
    }

    let path = CString::new(mount_root.as_os_str().as_bytes()).ok()?;
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = UuidBuffer { length: 0, uuid: [0; 16] };
    let result = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            &mut attrs as *mut libc::attrlist as *mut libc::c_void,
            &mut buffer as *mut UuidBuffer as *mut libc::c_void,
            std::mem::size_of::<UuidBuffer>(),
            0,
        )
    };
    if result != 0 || buffer.length < std::mem::size_of::<UuidBuffer>() as u32 {
        return None;
    }
    (!buffer.uuid.iter().all(|byte| *byte == 0)).then_some(buffer.uuid)
}

#[cfg(target_os = "macos")]
fn format_uuid(prefix: &str, uuid: &[u8; 16]) -> String {
    format!(
        "{prefix}:{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        uuid[0],
        uuid[1],
        uuid[2],
        uuid[3],
        uuid[4],
        uuid[5],
        uuid[6],
        uuid[7],
        uuid[8],
        uuid[9],
        uuid[10],
        uuid[11],
        uuid[12],
        uuid[13],
        uuid[14],
        uuid[15],
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_identity(canonical: &Path) -> PlatformIdentity {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let dev = std::fs::metadata(canonical).ok().map(|metadata| metadata.dev());
    let mount_root = device_root(canonical, dev);
    let relative = canonical.strip_prefix(&mount_root).unwrap_or(canonical);
    PlatformIdentity {
        volume: dev
            .map(|value| format!("unix-dev:{value}").into_bytes())
            .unwrap_or_else(|| b"unix-volume:unknown".to_vec()),
        relative_root: relative.as_os_str().as_bytes().to_vec(),
        file_ids_stable: unix_file_ids_stable(canonical),
    }
}

#[cfg(unix)]
fn device_root(path: &Path, device: Option<u64>) -> PathBuf {
    use std::os::unix::fs::MetadataExt;

    let Some(device) = device else {
        return path.to_path_buf();
    };
    let mut current = path.to_path_buf();
    while let Some(parent) = current.parent() {
        if parent == current {
            break;
        }
        match std::fs::metadata(parent) {
            Ok(metadata) if metadata.dev() == device => current = parent.to_path_buf(),
            _ => break,
        }
    }
    current
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn unix_file_ids_stable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    const MSDOS_SUPER_MAGIC: u64 = 0x4d44;
    const EXFAT_SUPER_MAGIC: u64 = 0x2011_bab0;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
        return true;
    }
    !matches!(info.f_type as u64, MSDOS_SUPER_MAGIC | EXFAT_SUPER_MAGIC)
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "linux"),
    not(target_os = "android")
))]
fn unix_file_ids_stable(_path: &Path) -> bool {
    true
}

#[cfg(windows)]
fn platform_identity(canonical: &Path) -> PlatformIdentity {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumePathNameW(
            file_name: *const u16,
            volume_path_name: *mut u16,
            buffer_length: u32,
        ) -> i32;
        fn GetVolumeInformationW(
            root_path_name: *const u16,
            volume_name_buffer: *mut u16,
            volume_name_size: u32,
            volume_serial_number: *mut u32,
            maximum_component_length: *mut u32,
            file_system_flags: *mut u32,
            file_system_name_buffer: *mut u16,
            file_system_name_size: u32,
        ) -> i32;
    }

    let canonical_wide: Vec<u16> = canonical.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut volume_buffer = vec![0u16; 32_768];
    let got_root = unsafe {
        GetVolumePathNameW(
            canonical_wide.as_ptr(),
            volume_buffer.as_mut_ptr(),
            volume_buffer.len() as u32,
        )
    } != 0;
    let volume_root = if got_root {
        let len = volume_buffer.iter().position(|value| *value == 0).unwrap_or(volume_buffer.len());
        String::from_utf16_lossy(&volume_buffer[..len])
    } else {
        windows_root_fallback(&canonical.to_string_lossy())
    };
    let normalized_volume_root = normalize_windows_path(&volume_root);
    let root_wide: Vec<u16> = std::ffi::OsStr::new(&volume_root).encode_wide().chain(Some(0)).collect();
    let mut serial = 0u32;
    let mut fs_name = [0u16; 64];
    let got_info = unsafe {
        GetVolumeInformationW(
            root_wide.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    } != 0;
    let fs_len = fs_name.iter().position(|value| *value == 0).unwrap_or(fs_name.len());
    let fs_name = String::from_utf16_lossy(&fs_name[..fs_len]);
    let normalized_root = normalize_windows_path(&canonical.to_string_lossy());
    let relative_root = normalized_root
        .strip_prefix(&normalized_volume_root)
        .unwrap_or(&normalized_root)
        .trim_start_matches('/')
        .as_bytes()
        .to_vec();
    let volume = if got_info {
        format!("windows-volume:{serial:08x}@{normalized_volume_root}").into_bytes()
    } else {
        format!("windows-root:{normalized_volume_root}").into_bytes()
    };
    PlatformIdentity {
        volume,
        relative_root,
        file_ids_stable: !fat_family(&fs_name),
    }
}

#[cfg(any(windows, test))]
fn normalize_windows_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = if let Some(rest) = path.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = path.strip_prefix("//?/") {
        rest.to_string()
    } else {
        path
    };
    path.trim_end_matches('/').to_lowercase()
}

#[cfg(windows)]
fn windows_root_fallback(path: &str) -> String {
    let normalized = normalize_windows_path(path);
    if let Some(rest) = normalized.strip_prefix("//") {
        let mut parts = rest.split('/');
        let host = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        if !host.is_empty() && !share.is_empty() {
            return format!("//{host}/{share}/");
        }
    }
    if normalized.as_bytes().get(1) == Some(&b':') {
        return format!("{}/", &normalized[..2]);
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "syncdash-local-state-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nested")).unwrap();
        root
    }

    #[test]
    fn binding_is_unambiguous_across_volume_and_relative_root_boundaries() {
        assert_ne!(encode_binding(b"ab", b"c"), encode_binding(b"a", b"bc"));
        assert_ne!(encode_binding(b"volume-a", b"same"), encode_binding(b"volume-b", b"same"));
        assert_ne!(encode_binding(b"same", b"root-a"), encode_binding(b"same", b"root-b"));
    }

    #[test]
    fn canonical_spellings_of_one_root_share_an_identity_but_siblings_do_not() {
        let root = temp_root("canonical");
        let direct = LocalScanStateIdentity::for_root(&root.join("nested"));
        let dotted = LocalScanStateIdentity::for_root(&root.join(".").join("nested"));
        let sibling = LocalScanStateIdentity::for_root(&root);
        assert_eq!(direct.binding(), dotted.binding());
        assert_ne!(direct.binding(), sibling.binding());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fat_family_file_ids_are_never_called_stable() {
        for name in ["FAT", "fat32", "msdos", "vfat", "exFAT"] {
            assert!(fat_family(name), "{name}");
        }
        for name in ["apfs", "NTFS", "ext4", "smbfs", ""] {
            assert!(!fat_family(name), "{name}");
        }
    }

    #[test]
    fn windows_path_normalization_folds_prefixes_separators_and_case() {
        assert_eq!(normalize_windows_path(r"\\?\D:\Code\"), "d:/code");
        assert_eq!(normalize_windows_path(r"\\?\UNC\Host\Share\Code"), "//host/share/code");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_the_volume_uuid_when_the_filesystem_reports_one() {
        let root = temp_root("macos-uuid");
        let canonical = std::fs::canonicalize(&root).unwrap();
        let (mount, _) = macos_mount(&canonical).unwrap();
        let identity = platform_identity(&canonical);
        if macos_volume_uuid(&mount).is_some() {
            assert!(identity.volume.starts_with(b"macos-uuid:"));
        } else {
            assert!(identity.volume.starts_with(b"macos-dev:"));
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
