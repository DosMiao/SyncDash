//! Translating an FTP listing entry into the VFS metadata shape.

use super::super::VMeta;
use super::super::VfsEntryKind;
use suppaftp::list::File as FtpFile;

pub(super) fn meta_of(f: &FtpFile) -> VMeta {
    let kind = if f.is_directory() {
        VfsEntryKind::Directory
    } else if f.is_symlink() {
        VfsEntryKind::Symlink
    } else {
        VfsEntryKind::File
    };
    VMeta {
        kind,
        size: f.size() as u64,
        mtime_ms: crate::foundation::time::systime_ms(f.modified()),
        mode: None,
        file_id: None,
        link: f.symlink().map(|p| p.to_string_lossy().into_owned()),
    }
}
