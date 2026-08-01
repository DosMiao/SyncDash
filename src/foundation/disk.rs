//! Free-space probe for a path.
//!
//! Lives at L0 because both ends of the layer graph need it and neither may reach the other:
//! `pipeline::guard` runs it as the pre-apply space gate, and `fs::vfs::local` answers
//! `free_space()` with it. It used to exist as two byte-identical copies, the one in `local.rs`
//! carrying a comment explaining that it was duplicated because `fs` must not reach up into
//! `pipeline`. Correct diagnosis, wrong remedy: `foundation` is the layer both may reach down to,
//! and two hand-maintained copies of an `unsafe` FFI call is exactly the thing it exists to end.

use std::path::Path;

/// `(bytes available to this user, total bytes on the volume)`.
///
/// `None` means the question could not be answered — the caller treats that as "no check
/// possible", never as "no space". The available figure is the caller's quota-aware number, not
/// the volume's raw free count; on a quota'd share those differ and the smaller one is the truth.
#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lp_directory_name: *const u16,
            lp_free_bytes_available_to_caller: *mut u64,
            lp_total_number_of_bytes: *mut u64,
            lp_total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }
    // UNC paths (\\host\share\...) are supported too — exactly what an SMB target needs
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 {
        None
    } else {
        Some((avail, total))
    }
}

#[cfg(unix)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        // f_frsize is the "fragment size", the right unit for capacity math; fall back to f_bsize when it is 0
        let unit = if st.f_frsize > 0 {
            st.f_frsize as u64
        } else {
            st.f_bsize as u64
        };
        Some((st.f_bavail as u64 * unit, st.f_blocks as u64 * unit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_sane_pair_for_the_temp_dir() {
        let (avail, total) =
            disk_space(&std::env::temp_dir()).expect("temp dir lives on a real volume");
        assert!(total > 0, "a mounted volume has a size");
        assert!(avail <= total, "available can never exceed total");
    }

    #[test]
    fn a_path_that_does_not_exist_answers_none_rather_than_zero() {
        let bogus = std::env::temp_dir()
            .join("syncdash-no-such-volume-xyzzy")
            .join("deeper");
        // Zero would read as "full disk" and block a run that has nothing wrong with it.
        assert_eq!(disk_space(&bogus), None);
    }
}
