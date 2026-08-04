//! The one place a `cap_primitives` metadata record becomes the VFS's own `VMeta`.
//!
//! Both the directory listing and the staged writer need this translation, which is why it is a
//! sibling of each rather than private to either. Keeping it single means the mtime, mode and
//! file-id a scan records cannot disagree with the ones a write reports back; the mode and
//! file-id spellings themselves come from `fs::meta`, the crate-wide owner.

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
        mode: crate::fs::meta::capability_unix_mode(metadata),
        file_id: crate::fs::meta::capability_file_id(metadata),
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
