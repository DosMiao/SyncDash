//! Volume identity on Linux.
//!
//! Linux can offer a durable answer — a filesystem UUID plus a unique mount instance — and falls
//! back to a process-scoped ephemeral identity when it cannot. The distinction matters because a
//! scan state bound to an ephemeral identity must not be reused after a restart.
//!
//! The module is ungated so its pure classification helpers stay compiled for the macOS test
//! suite; each item carries the narrowest gate its imports allow.

#[cfg(target_os = "linux")]
use super::PlatformIdentity;
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
pub(super) fn platform_identity(canonical: &Path) -> PlatformIdentity {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let dev = std::fs::metadata(canonical)
        .ok()
        .map(|metadata| metadata.dev());
    let mount_root = super::device_root(canonical, dev);
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
            linux_process_nonce(),
        ),
        relative_root: relative.as_os_str().as_bytes().to_vec(),
        file_ids_stable: linux_file_ids_stable(canonical),
        persistent_reuse: linux_identity_is_durable(
            filesystem_uuid.as_deref(),
            unique_mount.as_deref(),
        ),
    }
}

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
pub(super) fn normalize_linux_identifier(raw: &str) -> Option<String> {
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
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
pub(super) fn linux_volume_identity(
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

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
pub(super) fn linux_identity_is_durable(
    filesystem_uuid: Option<&str>,
    unique_mount: Option<&str>,
) -> bool {
    filesystem_uuid
        .and_then(normalize_linux_identifier)
        .is_some()
        || unique_mount.is_some_and(|value| !value.is_empty())
}

/// The wire spelling still says "unix": it is a persisted identity prefix, and renaming it would
/// orphan every cache generation bound under the old bytes.
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
pub(super) fn ephemeral_unix_volume_identity(dev: Option<u64>, process_nonce: &[u8]) -> Vec<u8> {
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
#[cfg(target_os = "linux")]
pub(super) fn linux_filesystem_uuid(dev: u64) -> Option<String> {
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

#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
pub(super) fn decode_mountinfo_path(field: &[u8]) -> Option<PathBuf> {
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
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
pub(super) fn mountinfo_mount_id(contents: &[u8], canonical: &Path) -> Option<u64> {
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

#[cfg(target_os = "linux")]
pub(super) fn linux_mount_instance(canonical: &Path, dev: Option<u64>) -> Option<String> {
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
#[cfg(target_os = "linux")]
pub(super) fn linux_unique_mount_instance(canonical: &Path, dev: Option<u64>) -> Option<String> {
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

#[cfg(target_os = "linux")]
pub(super) fn linux_process_nonce() -> &'static [u8] {
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

#[cfg(target_os = "linux")]
pub(super) fn linux_file_ids_stable(path: &Path) -> bool {
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
#[cfg(any(target_os = "linux", test))]
pub(super) fn linux_file_ids_stable_magic(magic: u64) -> bool {
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
