//! Whether a file's content is actually present on this disk.
//!
//! A cloud sync engine can leave a name and metadata behind while the bytes live on a server,
//! fetched on first read. A scan counts these so a compare can say up front that hashing the tree
//! would pull them down, instead of silently spending the user's bandwidth and disk.
//!
//! Exactly one arm compiles per target, mirroring the predicates `foundation::host` admits.

use super::LocalRoot;
use crate::foundation::path::RootRelativePath;

#[cfg(target_os = "macos")]
pub(super) fn is_dataless(
    root: &LocalRoot,
    relative: &RootRelativePath,
    _metadata: &cap_primitives::fs::Metadata,
) -> std::io::Result<bool> {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;

    // `st_flags` is not carried by the capability metadata record, so the file is opened and
    // asked. Opening a dataless file does not materialize it; only reading does.
    Ok(root.open_read(relative)?.metadata()?.st_flags() & SF_DATALESS != 0)
}

#[cfg(windows)]
pub(super) fn is_dataless(
    _root: &LocalRoot,
    _relative: &RootRelativePath,
    metadata: &cap_primitives::fs::Metadata,
) -> std::io::Result<bool> {
    use cap_primitives::fs::MetadataExt;

    // The answer must come from the listing attributes rather than from opening the file: a
    // RECALL_ON_OPEN placeholder hydrates on open, so probing that way would download the very
    // content the flag exists to announce.
    Ok(is_windows_placeholder(metadata.file_attributes()))
}

#[cfg(target_os = "linux")]
pub(super) fn is_dataless(
    _root: &LocalRoot,
    _relative: &RootRelativePath,
    _metadata: &cap_primitives::fs::Metadata,
) -> std::io::Result<bool> {
    // Linux cloud clients mount through FUSE and present complete files; the kernel exposes no
    // placeholder vocabulary to read here.
    Ok(false)
}

/// Whether Win32 file attributes mark content that is not resident on this disk.
///
/// Kept pure and compiled under `test` everywhere so the classification is checked on every host,
/// not only where the probe runs.
#[cfg(any(windows, test))]
pub(super) fn is_windows_placeholder(attributes: u32) -> bool {
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    attributes
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

#[cfg(test)]
mod tests {
    use super::is_windows_placeholder;

    #[test]
    fn placeholder_bits_are_flagged_and_resident_bits_are_not() {
        const ARCHIVE: u32 = 0x0000_0020;
        const NORMAL: u32 = 0x0000_0080;
        // OneDrive "always keep on this device". Content is fully resident, and calling it
        // dataless would misreport exactly the files the user pinned to avoid hydration stalls.
        const PINNED: u32 = 0x0008_0000;

        for resident in [0, ARCHIVE, NORMAL, ARCHIVE | PINNED] {
            assert!(!is_windows_placeholder(resident), "{resident:#x}");
        }
        for placeholder in [0x0000_1000, 0x0004_0000, 0x0040_0000] {
            assert!(
                is_windows_placeholder(ARCHIVE | placeholder),
                "{placeholder:#x}"
            );
        }
    }
}
