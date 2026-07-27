//! L0 filesystem primitives that own a lifecycle.
//!
//! `staged` is the atomic-write staging handle (same-directory temp file, fsync, rename);
//! `lock` is the root heartbeat lock. Named `fs::staged` rather than `atomic` because
//! `crate::atomic` read as `std::sync::atomic`, which several of these modules also import.
//! `vfs` is the virtual filesystem a sync root lives on — local disk today, SMB/SFTP/FTP
//! backends behind the same trait; its write side wraps `staged` rather than reimplementing it.

pub mod lock;
pub mod staged;
pub mod vfs;

use std::path::Path;

/// `remove_file`, clearing the read-only attribute on a PermissionDenied retry.
///
/// Git marks loose objects `r--r--r--`, and Windows (plus SMB servers honoring the
/// DOS attribute) refuses to delete such files — a real sync against a `.git`-carrying
/// tree failed thousands of deletes with os error 5 exactly this way. Both reference
/// projects carry this fallback (syncthing `Remove`: chmod 0600 then retry; FFS
/// `withPreparedTarget`: chmod 0666 then remove). On unix, unlink never needs write
/// permission on the file itself, so the retry simply never fires there.
pub fn remove_file_force(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let md = std::fs::symlink_metadata(p)?;
            if md.file_type().is_symlink() {
                // std has no lchmod; set_permissions would chmod the TARGET. A
                // permission-blocked symlink stays failed rather than risking that.
                return Err(e);
            }
            let mut perms = md.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            std::fs::set_permissions(p, perms)?;
            std::fs::remove_file(p)
        }
        r => r,
    }
}

/// `rename`, with the same read-only-clearing retry on the source (NTFS moves
/// read-only files fine, but an SMB server mapping unix modes may refuse).
pub fn rename_force(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            match std::fs::symlink_metadata(from) {
                Ok(md) if !md.file_type().is_symlink() => {
                    let mut perms = md.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = std::fs::set_permissions(from, perms);
                    std::fs::rename(from, to)
                }
                _ => Err(e),
            }
        }
        r => r,
    }
}
