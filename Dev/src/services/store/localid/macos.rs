//! Volume identity as macOS reports it.
//!
//! Every item keeps the `cfg` it had before this file existed; the module itself is ungated, so
//! the set of code compiled on each platform is unchanged.

#[cfg(target_os = "macos")]
use super::{unix::device_root, PlatformIdentity};
#[cfg(target_os = "macos")]
use crate::fs::vfs::local::volume::file_ids_stable_for_fs;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
pub(super) fn platform_identity(canonical: &Path) -> PlatformIdentity {
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
        file_ids_stable: file_ids_stable_for_fs(&fs_name),
        persistent_reuse: uuid.is_some(),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn macos_mount(path: &Path) -> Option<(PathBuf, String)> {
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
pub(super) fn macos_volume_uuid(mount_root: &Path) -> Option<[u8; 16]> {
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
pub(super) fn format_uuid(prefix: &str, uuid: &[u8; 16]) -> String {
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
