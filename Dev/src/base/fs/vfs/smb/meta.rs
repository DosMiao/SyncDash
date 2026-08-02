//! Translating SMB file information into the VFS metadata shape.

use super::super::VMeta;
use super::super::VfsEntryKind;

pub(super) fn meta_of(size: u64, is_dir: bool, modified_ticks: u64) -> VMeta {
    VMeta {
        kind: if is_dir {
            VfsEntryKind::Directory
        } else {
            VfsEntryKind::File
        },
        size,
        mtime_ms: super::basic_info::unix_ms_from_filetime(modified_ticks),
        // SMB2 carries DOS attributes, not a unix mode; inventing 0o644 here would be a lie
        // the engine cannot tell from a real one.
        mode: None,
        file_id: None,
        link: None,
    }
}
