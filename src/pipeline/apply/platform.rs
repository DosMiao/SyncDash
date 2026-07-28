//! The cfg-gated filesystem primitives apply needs: read and write an mtime, write a unix mode,
//! and test existence without following a symlink.
//!
//! Separated because every one of them is a two-arm cfg, and interleaving those with the
//! execution logic made both harder to read than either is alone.

use std::path::Path;

use filetime::FileTime;

pub(super) fn set_mtime(path: &Path, mtime_ms: i64) {
    let ft = FileTime::from_unix_time(mtime_ms / 1000, ((mtime_ms % 1000) * 1_000_000) as u32);
    let _ = filetime::set_file_mtime(path, ft);
}

/// Read a file's mtime (unix milliseconds). None only means "could not read it" (metadata/modified failed);
/// the caller uses that to decide "then don't set the mtime". The conversion itself is left to `foundation::time::systime_ms`.
pub(super) fn read_mtime_ms(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(crate::foundation::time::systime_ms(t))
}

#[cfg(unix)]
pub(super) fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
pub(super) fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    // Windows has no unix permission bits. A plan carrying a mode can only arise between unix↔unix,
    // so reaching here means the executing side is Windows — skip silently rather than error.
    Ok(())
}

pub(super) fn exists_no_follow(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}
