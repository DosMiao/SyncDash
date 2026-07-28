//! L0 filesystem primitives that own a lifecycle.
//!
//! `staged` is the atomic-write staging handle (same-directory temp file, fsync, rename);
//! `lock` is the root heartbeat lock. Named `fs::staged` rather than `atomic` because
//! `crate::atomic` read as `std::sync::atomic`, which several of these modules also import.
//! `vfs` is the virtual filesystem a sync root lives on — local disk today, SMB/SFTP/FTP
//! backends behind the same trait; its write side wraps `staged` rather than reimplementing it.
//! `ssh` is the authenticated session both things that ride ssh share: the `sftp://` backend and
//! `transfer::peer`'s peer lane, which used to reach the same hosts with the same keys by two
//! different means.

pub mod lock;
pub mod ssh;
pub mod staged;
pub mod vfs;

use std::path::Path;

/// `remove_file`, clearing the read-only attribute on a PermissionDenied retry — **on Windows**.
///
/// Git marks loose objects `r--r--r--`, and Windows (plus SMB servers honoring the
/// DOS attribute) refuses to delete such files — a real sync against a `.git`-carrying
/// tree failed thousands of deletes with os error 5 exactly this way. Both reference
/// projects carry this fallback (syncthing `Remove`: chmod 0600 then retry; FFS
/// `withPreparedTarget`: chmod 0666 then remove).
///
/// The retry is Windows-only because it is only *correct* on Windows, where the read-only DOS
/// attribute really is the cause and clearing it really is the remedy. This module used to claim
/// the retry "never fires" on unix, which was wrong about the trigger: `unlink` returns EACCES
/// when the **parent directory** is not writable, under a sticky-bit directory you do not own, or
/// on a `chflags uchg` file — measured here, errno 13. And on unix `set_readonly(false)` is not an
/// attribute, it is `mode |= 0o222`: measured, 0600 becomes 0622. So in the commonest case the
/// chmod succeeded (you own the file), the retry failed again (the parent is still not writable),
/// and the file was left group- and world-writable permanently. Mirroring into `/Users/Shared` or
/// another user's tree widened one more file on every failed delete.
///
/// On unix the original PermissionDenied now propagates untouched. That is both the loud failure
/// and the honest diagnosis: the file's own mode was never the problem.
pub fn remove_file_force(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(p) {
        #[cfg(windows)]
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let md = std::fs::symlink_metadata(p)?;
            if md.file_type().is_symlink() {
                // std has no lchmod; set_permissions would chmod the TARGET. A
                // permission-blocked symlink stays failed rather than risking that.
                return Err(e);
            }
            let mut perms = md.permissions();
            perms.set_readonly(false);
            std::fs::set_permissions(p, perms)?;
            std::fs::remove_file(p)
        }
        r => r,
    }
}

/// `remove_dir`, clearing the read-only attribute on a PermissionDenied retry.
///
/// The directory half of [`remove_file_force`], and it is a distinct case: on Windows the
/// read-only attribute on a *directory* is nominally a "this folder is customized" flag, but
/// `RemoveDirectory` still refuses it. Measured on Win11 26200: `remove_dir` over an empty
/// directory carrying the attribute fails with `PermissionDenied` (os error 5), the same
/// wording users saw on the read-only file case.
///
/// (`std::fs::remove_dir_all` needs no such helper — it clears the attribute itself, verified
/// over a tree holding both a read-only file and a read-only directory.)
///
/// Windows-only for the same reason as [`remove_file_force`]: on unix this turned 0755 into 0777.
pub fn remove_dir_force(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir(p) {
        #[cfg(windows)]
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let Ok(md) = std::fs::symlink_metadata(p) else { return Err(e) };
            let mut perms = md.permissions();
            perms.set_readonly(false);
            std::fs::set_permissions(p, perms)?;
            std::fs::remove_dir(p)
        }
        r => r,
    }
}

/// `rename`, with the same read-only-clearing retry on the source (NTFS moves
/// read-only files fine, but an SMB server mapping unix modes may refuse).
///
/// Windows-only, as above — and this one hid the widening the hardest, because the chmod's failure
/// was discarded with `let _ =`.
pub fn rename_force(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        #[cfg(windows)]
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            match std::fs::symlink_metadata(from) {
                Ok(md) if !md.file_type().is_symlink() => {
                    let mut perms = md.permissions();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn set_ro(p: &Path, ro: bool) {
        let mut perms = std::fs::metadata(p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(ro);
        std::fs::set_permissions(p, perms).unwrap();
    }

    /// The directory counterpart of the read-only file failure. On Windows the retry really
    /// fires (plain `remove_dir` returns os error 5 here); on unix the first call already
    /// succeeds, so this asserts the behavior rather than the mechanism on both.
    #[test]
    fn a_read_only_directory_can_still_be_removed() {
        let base = std::env::temp_dir().join(format!("syncdash-rodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let d = base.join("locked-dir");
        std::fs::create_dir_all(&d).unwrap();
        set_ro(&d, true);

        #[cfg(windows)]
        {
            let e = std::fs::remove_dir(&d).expect_err("premise: Windows refuses a read-only directory");
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
        }
        remove_dir_force(&d).expect("force removal must succeed");
        assert!(!d.exists());

        // A non-empty directory must still report NotEmpty rather than being force-emptied
        let full = base.join("full");
        std::fs::create_dir_all(&full).unwrap();
        std::fs::write(full.join("x"), b"x").unwrap();
        assert!(remove_dir_force(&full).is_err(), "force must not turn into a recursive delete");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The read-only file case, at the primitive level (the apply-lane regression test covers
    /// the same ground end to end through trash and versioning).
    #[test]
    fn a_read_only_file_can_still_be_removed_and_renamed() {
        let base = std::env::temp_dir().join(format!("syncdash-rofile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let a = base.join("a.txt");
        std::fs::write(&a, b"git-loose-object").unwrap();
        set_ro(&a, true);
        let moved = base.join("b.txt");
        rename_force(&a, &moved).expect("rename must survive the read-only source");
        remove_file_force(&moved).expect("removal must survive the read-only attribute");
        assert!(!moved.exists());

        let _ = std::fs::remove_dir_all(&base);
    }
}
