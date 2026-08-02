//! Deciding whether this root can be walked in bulk at all.
//!
//! Some filesystems answer `getattrlistbulk` with an error that means "not supported here" rather
//! than "this call failed". Telling the two apart is what lets the scan fall back to the ordinary
//! walk instead of reporting the root as unreadable.

use super::record::ATTRS;
use std::os::fd::RawFd;
use std::path::PathBuf;

#[derive(Debug)]
pub(super) struct RootBulkCompatibilityError {
    pub(super) root: PathBuf,
    pub(super) source: std::io::Error,
}

impl std::fmt::Display for RootBulkCompatibilityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "scan of '{}' could not use bulk directory metadata: {}",
            self.root.display(),
            self.source
        )
    }
}

impl std::error::Error for RootBulkCompatibilityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub(super) fn is_root_bulk_compatibility_error(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<RootBulkCompatibilityError>())
        .is_some()
}

pub(super) fn system_bulk_read(fd: RawFd, buffer: &mut [u8]) -> std::io::Result<usize> {
    let count = unsafe {
        // SAFETY: `fd` names a readable directory, ATTRS is fully initialized, and `buffer` is
        // valid writable memory for the supplied length.
        libc::getattrlistbulk(
            fd,
            &ATTRS as *const libc::attrlist as *mut libc::c_void,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            libc::FSOPT_NOFOLLOW as u64,
        )
    };
    if count < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(count as usize)
    }
}
