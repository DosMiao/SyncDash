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
    persistent_reuse: bool,
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
            persistent_reuse: platform.persistent_reuse,
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

    /// Whether the platform supplied a volume identity that remains trustworthy across process
    /// restarts and media replacement. Callers must not read or write on-disk scan state otherwise.
    pub(crate) fn persistent_reuse(&self) -> bool {
        self.persistent_reuse
    }

    #[cfg(test)]
    pub(crate) fn injected(
        cache_key: impl Into<String>,
        binding: impl Into<Vec<u8>>,
        persistent_reuse: bool,
    ) -> LocalScanStateIdentity {
        LocalScanStateIdentity {
            cache_key: cache_key.into(),
            binding: binding.into(),
            file_ids_stable: false,
            persistent_reuse,
        }
    }
}

#[derive(Debug)]
struct PlatformIdentity {
    volume: Vec<u8>,
    relative_root: Vec<u8>,
    file_ids_stable: bool,
    persistent_reuse: bool,
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

#[cfg(test)]
fn fat_family(fs_name: &str) -> bool {
    matches!(
        fs_name.to_ascii_lowercase().as_str(),
        "fat" | "fat12" | "fat16" | "fat32" | "msdos" | "vfat" | "exfat"
    )
}

fn named_filesystem_has_stable_file_ids(fs_name: &str) -> bool {
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

#[cfg(target_os = "macos")]
fn platform_identity(canonical: &Path) -> PlatformIdentity {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let dev = std::fs::metadata(canonical)
        .ok()
        .map(|metadata| metadata.dev());
    let (mount_root, fs_name) =
        macos_mount(canonical).unwrap_or_else(|| (device_root(canonical, dev), String::new()));
    let uuid = macos_volume_uuid(&mount_root);
    let volume = uuid
        .map(|uuid| format_uuid("macos-uuid", &uuid).into_bytes())
        .unwrap_or_else(|| {
            format!("macos-nondurable-dev:{}", dev.unwrap_or(u64::MAX)).into_bytes()
        });
    let relative = canonical.strip_prefix(&mount_root).unwrap_or(canonical);
    let relative_root = relative.as_os_str().as_bytes().to_vec();

    PlatformIdentity {
        volume,
        relative_root,
        file_ids_stable: named_filesystem_has_stable_file_ids(&fs_name),
        persistent_reuse: uuid.is_some(),
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
    let mut buffer = UuidBuffer {
        length: 0,
        uuid: [0; 16],
    };
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

#[cfg(any(target_os = "linux", target_os = "android"))]
fn platform_identity(canonical: &Path) -> PlatformIdentity {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let dev = std::fs::metadata(canonical)
        .ok()
        .map(|metadata| metadata.dev());
    let mount_root = device_root(canonical, dev);
    let relative = canonical.strip_prefix(&mount_root).unwrap_or(canonical);
    let filesystem_uuid = dev.and_then(linux_filesystem_uuid);
    let unique_mount = linux_unique_mount_instance(canonical, dev);
    let ordinary_mount = unique_mount
        .is_none()
        .then(|| linux_mount_instance(canonical, dev))
        .flatten();
    PlatformIdentity {
        volume: linux_volume_identity(
            filesystem_uuid.as_deref(),
            unique_mount.as_deref().or(ordinary_mount.as_deref()),
            dev,
            unix_process_nonce(),
        ),
        relative_root: relative.as_os_str().as_bytes().to_vec(),
        file_ids_stable: unix_file_ids_stable(canonical),
        persistent_reuse: linux_identity_is_durable(
            filesystem_uuid.as_deref(),
            unique_mount.as_deref(),
        ),
    }
}

/// Other Unix targets do not yet have a portable filesystem-UUID probe here. Include a process
/// nonce so their reusable device number is never treated as durable replacement-media evidence.
/// This deliberately sacrifices cross-process cache reuse instead of accepting state from another
/// filesystem that later acquired the same `st_dev` value.
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "linux"),
    not(target_os = "android")
))]
fn platform_identity(canonical: &Path) -> PlatformIdentity {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let dev = std::fs::metadata(canonical)
        .ok()
        .map(|metadata| metadata.dev());
    let mount_root = device_root(canonical, dev);
    let relative = canonical.strip_prefix(&mount_root).unwrap_or(canonical);
    PlatformIdentity {
        volume: ephemeral_unix_volume_identity(dev, unix_process_nonce()),
        relative_root: relative.as_os_str().as_bytes().to_vec(),
        file_ids_stable: unix_file_ids_stable(canonical),
        persistent_reuse: false,
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    all(test, target_os = "macos")
))]
fn normalize_linux_identifier(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')))
    .then(|| value.to_ascii_lowercase())
}

/// Select Linux's durable volume evidence. `st_dev` participates only in discovering the UUID and
/// distinguishing a live mount instance; it is never accepted by itself because its major/minor
/// pair can be reused when removable media is replaced.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    all(test, target_os = "macos")
))]
fn linux_volume_identity(
    filesystem_uuid: Option<&str>,
    mount_instance: Option<&str>,
    dev: Option<u64>,
    process_nonce: &[u8],
) -> Vec<u8> {
    if let Some(uuid) = filesystem_uuid.and_then(normalize_linux_identifier) {
        return format!("linux-fs-uuid:{uuid}").into_bytes();
    }
    if let Some(instance) = mount_instance.filter(|value| !value.is_empty()) {
        return format!("linux-mount-instance:{instance}").into_bytes();
    }
    ephemeral_unix_volume_identity(dev, process_nonce)
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    all(test, target_os = "macos")
))]
fn linux_identity_is_durable(filesystem_uuid: Option<&str>, unique_mount: Option<&str>) -> bool {
    filesystem_uuid
        .and_then(normalize_linux_identifier)
        .is_some()
        || unique_mount.is_some_and(|value| !value.is_empty())
}

#[cfg(any(all(unix, not(target_os = "macos")), all(test, target_os = "macos")))]
fn ephemeral_unix_volume_identity(dev: Option<u64>, process_nonce: &[u8]) -> Vec<u8> {
    let mut volume = Vec::with_capacity(40 + process_nonce.len());
    volume.extend_from_slice(b"unix-ephemeral-volume\0");
    volume.extend_from_slice(&(process_nonce.len() as u64).to_le_bytes());
    volume.extend_from_slice(process_nonce);
    volume.extend_from_slice(&dev.unwrap_or(u64::MAX).to_le_bytes());
    volume
}

/// Resolve the mounted block device through udev's filesystem-UUID aliases. Reading the symlink's
/// target metadata compares its `rdev` with the mounted root's `st_dev`, so the mount source's
/// spelling (`/dev/sda1`, `/dev/mapper/...`, and so on) is irrelevant.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn linux_filesystem_uuid(dev: u64) -> Option<String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let mut matches = Vec::new();
    for entry in std::fs::read_dir("/dev/disk/by-uuid").ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if !metadata.file_type().is_block_device() || metadata.rdev() != dev {
            continue;
        }
        let Some(uuid) = entry
            .file_name()
            .to_str()
            .and_then(normalize_linux_identifier)
        else {
            continue;
        };
        matches.push(uuid);
    }
    matches.sort_unstable();
    matches.dedup();
    (!matches.is_empty()).then(|| matches.join(":"))
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    all(test, target_os = "macos")
))]
fn decode_mountinfo_path(field: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] != b'\\' {
            decoded.push(field[index]);
            index += 1;
            continue;
        }
        let digits = field.get(index + 1..index + 4)?;
        if !digits.iter().all(|digit| matches!(digit, b'0'..=b'7')) {
            return None;
        }
        decoded.push((digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + digits[2] - b'0');
        index += 4;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
}

/// Return the visible mount with the longest component prefix. Linux escapes whitespace and
/// backslashes in mountinfo fields as octal bytes, which must be decoded before path matching.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    all(test, target_os = "macos")
))]
fn mountinfo_mount_id(contents: &[u8], canonical: &Path) -> Option<u64> {
    let mut best = None;
    for line in contents.split(|byte| *byte == b'\n') {
        let fields: Vec<&[u8]> = line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect();
        if fields.len() < 7 || !fields.iter().any(|field| *field == b"-") {
            continue;
        }
        let Some(id) = std::str::from_utf8(fields[0])
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        let Some(mount_point) = decode_mountinfo_path(fields[4]) else {
            continue;
        };
        if !canonical.starts_with(&mount_point) {
            continue;
        }
        let depth = mount_point.components().count();
        if best.is_none_or(|(best_depth, _)| depth >= best_depth) {
            best = Some((depth, id));
        }
    }
    best.map(|(_, id)| id)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn linux_mount_instance(canonical: &Path, dev: Option<u64>) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let dev = dev?;
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .and_then(|value| normalize_linux_identifier(&value))?;
    let namespace = std::fs::metadata("/proc/self/ns/mnt").ok()?.ino();
    let mountinfo = std::fs::read("/proc/self/mountinfo").ok()?;
    let mount_id = mountinfo_mount_id(&mountinfo, canonical)?;
    Some(format!(
        "boot={boot_id};ns={namespace};mount={mount_id};dev={dev}"
    ))
}

/// Linux 6.8 added a mount identifier that is never reused during one boot. Pair it with boot_id
/// so the value is also unambiguous after reboot. Older kernels report the request bit absent (or
/// reject it); ordinary mountinfo IDs remain useful diagnostics but are not durable cache evidence.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn linux_unique_mount_instance(canonical: &Path, dev: Option<u64>) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;

    const STATX_MNT_ID_UNIQUE: u32 = 0x4000;
    let path = std::ffi::CString::new(canonical.as_os_str().as_bytes()).ok()?;
    let mut info: libc::statx = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::statx(
            libc::AT_FDCWD,
            path.as_ptr(),
            0,
            STATX_MNT_ID_UNIQUE,
            &mut info,
        )
    } != 0
        || info.stx_mask & STATX_MNT_ID_UNIQUE == 0
    {
        return None;
    }
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .and_then(|value| normalize_linux_identifier(&value))?;
    Some(format!(
        "boot={boot_id};unique_mount={};dev={}",
        info.stx_mnt_id,
        dev.unwrap_or(u64::MAX)
    ))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_process_nonce() -> &'static [u8] {
    use std::io::Read;
    use std::sync::OnceLock;

    static NONCE: OnceLock<Vec<u8>> = OnceLock::new();
    NONCE
        .get_or_init(|| {
            let mut random = vec![0u8; 32];
            if std::fs::File::open("/dev/urandom")
                .and_then(|mut file| file.read_exact(&mut random))
                .is_ok()
            {
                return random;
            }
            let mut seed = format!(
                "pid={};time={:?}",
                std::process::id(),
                std::time::SystemTime::now()
            )
            .into_bytes();
            seed.extend_from_slice(format!(";address={:p}", &NONCE).as_bytes());
            blake3::hash(&seed).as_bytes().to_vec()
        })
        .as_slice()
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

    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut info) } != 0 {
        return false;
    }
    // `f_type` is a signed `long` on some targets even though Linux magic values are
    // conventionally written as unsigned 32-bit constants. Normalize through `u32` so
    // Btrfs' high bit does not get sign-extended on a 32-bit build.
    linux_file_ids_stable_magic((info.f_type as u32) as u64)
}

/// Filesystems for which Linux exposes object numbers with stable inode semantics. This must be
/// a positive list: overlay/network/FUSE and future filesystems remain ineligible until their
/// identity guarantees are understood, rather than silently becoming rename evidence.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn linux_file_ids_stable_magic(magic: u64) -> bool {
    matches!(
        magic,
        0x0000_ef53 // ext2/3/4
            | 0x9123_683e // btrfs
            | 0x5846_5342 // xfs
            | 0x2fc1_2fc1 // zfs
            | 0xf2f5_2010 // f2fs
            | 0x3153_464a // jfs
    )
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "linux"),
    not(target_os = "android")
))]
fn unix_file_ids_stable(_path: &Path) -> bool {
    false
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
        fn GetVolumeNameForVolumeMountPointW(
            volume_mount_point: *const u16,
            volume_name: *mut u16,
            buffer_length: u32,
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
        let len = volume_buffer
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(volume_buffer.len());
        String::from_utf16_lossy(&volume_buffer[..len])
    } else {
        windows_root_fallback(&canonical.to_string_lossy())
    };
    let normalized_volume_root = normalize_windows_path(&volume_root);
    let root_wide: Vec<u16> = std::ffi::OsStr::new(&volume_root)
        .encode_wide()
        .chain(Some(0))
        .collect();
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
    let fs_len = fs_name
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(fs_name.len());
    let fs_name = String::from_utf16_lossy(&fs_name[..fs_len]);
    let mut unique_buffer = vec![0u16; 32_768];
    let got_unique = got_root
        && unsafe {
            GetVolumeNameForVolumeMountPointW(
                root_wide.as_ptr(),
                unique_buffer.as_mut_ptr(),
                unique_buffer.len() as u32,
            )
        } != 0;
    let unique_name = got_unique.then(|| {
        let len = unique_buffer
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(unique_buffer.len());
        String::from_utf16_lossy(&unique_buffer[..len])
    });
    let normalized_root = normalize_windows_path(&canonical.to_string_lossy());
    let relative_root = normalized_root
        .strip_prefix(&normalized_volume_root)
        .unwrap_or(&normalized_root)
        .trim_start_matches('/')
        .as_bytes()
        .to_vec();
    let (volume, persistent_reuse) = windows_volume_identity(
        unique_name.as_deref(),
        got_info.then_some(serial),
        &normalized_volume_root,
    );
    PlatformIdentity {
        volume,
        relative_root,
        file_ids_stable: got_info && named_filesystem_has_stable_file_ids(&fs_name),
        persistent_reuse,
    }
}

#[cfg(any(windows, test))]
fn windows_volume_identity(
    unique_name: Option<&str>,
    serial: Option<u32>,
    normalized_volume_root: &str,
) -> (Vec<u8>, bool) {
    let unique = unique_name
        .map(normalize_windows_path)
        .filter(|value| !value.is_empty());
    match unique {
        Some(unique) => (
            format!(
                "windows-volume-guid:{unique};serial={}",
                serial.map_or_else(|| "unknown".to_string(), |value| format!("{value:08x}"))
            )
            .into_bytes(),
            true,
        ),
        None => (
            format!("windows-nondurable-root:{normalized_volume_root}").into_bytes(),
            false,
        ),
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
        let root =
            std::env::temp_dir().join(format!("syncdash-local-state-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nested")).unwrap();
        root
    }

    #[test]
    fn binding_is_unambiguous_across_volume_and_relative_root_boundaries() {
        assert_ne!(encode_binding(b"ab", b"c"), encode_binding(b"a", b"bc"));
        assert_ne!(
            encode_binding(b"volume-a", b"same"),
            encode_binding(b"volume-b", b"same")
        );
        assert_ne!(
            encode_binding(b"same", b"root-a"),
            encode_binding(b"same", b"root-b")
        );
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
        assert!(named_filesystem_has_stable_file_ids("apfs"));
        assert!(named_filesystem_has_stable_file_ids("NTFS"));
        assert!(named_filesystem_has_stable_file_ids("ext4"));
        assert!(!named_filesystem_has_stable_file_ids("exFAT"));
        assert!(!named_filesystem_has_stable_file_ids(""));
        assert!(!named_filesystem_has_stable_file_ids("mysteryfs"));
    }

    #[test]
    fn linux_file_id_stability_is_a_positive_magic_allowlist() {
        for magic in [
            0x0000_ef53,
            0x9123_683e,
            0x5846_5342,
            0x2fc1_2fc1,
            0xf2f5_2010,
            0x3153_464a,
        ] {
            assert!(linux_file_ids_stable_magic(magic), "magic {magic:#x}");
        }
        for magic in [
            0x0000_4d44, // FAT
            0x2011_bab0, // exFAT
            0x794c_7630, // overlayfs
            0x0102_1994, // tmpfs
            0x6573_5546, // FUSE
            0xfeed_beef, // unknown/future
        ] {
            assert!(!linux_file_ids_stable_magic(magic), "magic {magic:#x}");
        }
    }

    #[test]
    fn windows_path_normalization_folds_prefixes_separators_and_case() {
        assert_eq!(normalize_windows_path(r"\\?\D:\Code\"), "d:/code");
        assert_eq!(
            normalize_windows_path(r"\\?\UNC\Host\Share\Code"),
            "//host/share/code"
        );
    }

    #[test]
    fn windows_requires_a_volume_guid_even_when_serial_and_mount_path_match() {
        let (first, first_durable) =
            windows_volume_identity(Some(r"\\?\Volume{AAAA}\"), Some(7), "e:");
        let (replacement, replacement_durable) =
            windows_volume_identity(Some(r"\\?\Volume{BBBB}\"), Some(7), "e:");
        let (unc_or_probe_failure, fallback_durable) = windows_volume_identity(None, Some(7), "e:");

        assert!(first_durable && replacement_durable);
        assert_ne!(first, replacement);
        assert!(!fallback_durable);
        assert!(unc_or_probe_failure.starts_with(b"windows-nondurable-root:"));
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    fn linux_mountinfo_parser_decodes_paths_and_chooses_the_deepest_mount() {
        let mountinfo = b"not a mountinfo row\n\
25 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw\n\
40 25 8:2 / /media/Backup\\040Disk rw,relatime - exfat /dev/sdb1 rw\n\
41 40 8:2 /nested /media/Backup\\040Disk/nested rw,relatime - exfat /dev/sdb1 rw\n";

        assert_eq!(
            mountinfo_mount_id(mountinfo, Path::new("/media/Backup Disk/file.txt")),
            Some(40)
        );
        assert_eq!(
            mountinfo_mount_id(
                mountinfo,
                Path::new("/media/Backup Disk/nested/deeper/file.txt")
            ),
            Some(41)
        );
        assert_eq!(
            decode_mountinfo_path(br"/media/has\134slash"),
            Some(PathBuf::from("/media/has\\slash"))
        );
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    fn linux_prefers_a_persistent_filesystem_uuid_over_mount_numbers() {
        let first = linux_volume_identity(
            Some("A1B2-C3D4"),
            Some("boot=old;ns=1;mount=10;dev=8"),
            Some(8),
            b"first-process",
        );
        let same_filesystem_after_remount = linux_volume_identity(
            Some("a1b2-c3d4"),
            Some("boot=new;ns=2;mount=99;dev=42"),
            Some(42),
            b"second-process",
        );
        let replacement = linux_volume_identity(
            Some("ffff-0001"),
            Some("boot=old;ns=1;mount=10;dev=8"),
            Some(8),
            b"first-process",
        );

        assert_eq!(first, same_filesystem_after_remount);
        assert_ne!(first, replacement);
        assert!(first.starts_with(b"linux-fs-uuid:"));
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    fn linux_fallbacks_fail_closed_when_st_dev_is_reused() {
        let mounted = linux_volume_identity(
            None,
            Some("boot=a;ns=1;mount=10;dev=8"),
            Some(8),
            b"same-process",
        );
        let replacement_mount = linux_volume_identity(
            None,
            Some("boot=a;ns=1;mount=11;dev=8"),
            Some(8),
            b"same-process",
        );
        let after_reboot = linux_volume_identity(
            None,
            Some("boot=b;ns=1;mount=10;dev=8"),
            Some(8),
            b"same-process",
        );
        let same_mount_from_another_process = linux_volume_identity(
            None,
            Some("boot=a;ns=1;mount=10;dev=8"),
            Some(8),
            b"another-process",
        );
        assert_eq!(mounted, same_mount_from_another_process);
        assert_ne!(mounted, replacement_mount);
        assert_ne!(mounted, after_reboot);

        let no_mount_evidence_a = linux_volume_identity(None, None, Some(8), b"process-a");
        let no_mount_evidence_b = linux_volume_identity(None, None, Some(8), b"process-b");
        assert_ne!(no_mount_evidence_a, no_mount_evidence_b);
        assert!(no_mount_evidence_a.starts_with(b"unix-ephemeral-volume\0"));
        assert!(!no_mount_evidence_a
            .windows(b"unix-dev:".len())
            .any(|part| part == b"unix-dev:"));

        // Ordinary mountinfo IDs and st_dev can both be reused after unmount. Even if their bytes
        // happen to collide, the durable gate prevents the old on-disk cache from being consulted.
        assert!(!linux_identity_is_durable(None, None));
        assert!(!linux_identity_is_durable(None, Some("")));
        assert!(linux_identity_is_durable(
            None,
            Some("boot=a;unique_mount=10;dev=8")
        ));
        assert!(linux_identity_is_durable(Some("A1B2-C3D4"), None));
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
            assert!(identity.persistent_reuse);
        } else {
            assert!(identity.volume.starts_with(b"macos-nondurable-dev:"));
            assert!(!identity.persistent_reuse);
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
