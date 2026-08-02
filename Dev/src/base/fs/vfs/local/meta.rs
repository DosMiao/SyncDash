//! The one place a `cap_primitives` metadata record becomes the VFS's own `VMeta`.
//!
//! Both the directory listing and the staged writer need this translation, which is why it is a
//! sibling of each rather than private to either. Keeping it single means the mtime, mode and
//! file-id a scan records cannot disagree with the ones a write reports back.

use super::super::{VMeta, VfsEntryKind};

pub(super) fn meta_of(metadata: &cap_primitives::fs::Metadata) -> VMeta {
    let kind = if metadata.is_symlink() {
        VfsEntryKind::Symlink
    } else if metadata.is_dir() {
        VfsEntryKind::Directory
    } else {
        VfsEntryKind::File
    };
    VMeta {
        kind,
        size: metadata.len(),
        mtime_ms: metadata_mtime_ms(metadata),
        mode: mode_of(metadata),
        file_id: file_id_of(metadata),
        link: None,
    }
}

pub(super) fn metadata_mtime_ms(metadata: &cap_primitives::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .map(|time| crate::foundation::time::systime_ms(time.into_std()))
        .unwrap_or(0)
}

#[cfg(unix)]
pub(super) fn mode_of(metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    use cap_primitives::fs::MetadataExt;
    Some(metadata.mode() & 0o7777)
}

#[cfg(not(unix))]
pub(super) fn mode_of(_metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
pub(super) fn file_id_of(metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    use cap_primitives::fs::MetadataExt;
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
pub(super) fn file_id_of(_metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    None
}
