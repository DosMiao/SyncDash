//! Volume identity as Windows reports it.
//!
//! Every item keeps the `cfg` it had before this file existed; the module itself is ungated.

#[cfg(windows)]
use super::{named_filesystem_has_stable_file_ids, PlatformIdentity};
#[cfg(windows)]
use std::path::Path;

#[cfg(windows)]
pub(super) fn platform_identity(canonical: &Path) -> PlatformIdentity {
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
pub(super) fn windows_volume_identity(
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
pub(super) fn normalize_windows_path(path: &str) -> String {
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
pub(super) fn windows_root_fallback(path: &str) -> String {
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
