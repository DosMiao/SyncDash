//! Volume identity: whether two paths live on the same storage device.
//!
//! This answers a question the apply and compare lanes ask about *paths*, not about a backend, and
//! it decides where preserved originals go. Trash and versioning both move a file aside by rename,
//! which only works within one device; when the configured store is on another volume the lane has
//! to fall back to an in-root location instead. Getting the answer wrong in the permissive
//! direction turns a preservation into a failed rename at the moment a file is about to be
//! overwritten.
//!
//! It lived inside the local VFS backend, so every caller that needed it reached into a backend
//! for a fact about the filesystem in general. The volume-*classification* tables (medium, case
//! sensitivity, mtime precision) stay with that backend, because those come from probing a
//! mounted filesystem; this one is pure path and metadata reasoning.

use std::path::Path;

/// Whether both paths resolve onto the same device.
///
/// A path that does not exist yet resolves through its nearest existing ancestor: the caller is
/// usually asking about a directory it is *about* to create.
#[cfg(unix)]
pub fn same_device(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    fn device(path: &Path) -> Option<u64> {
        let mut candidate = Some(path);
        while let Some(path) = candidate {
            match std::fs::metadata(path) {
                Ok(metadata) => return Some(metadata.dev()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    candidate = path.parent();
                }
                Err(_) => return None,
            }
        }
        None
    }

    matches!((device(left), device(right)), (Some(left), Some(right)) if left == right)
}

/// Whether both paths resolve onto the same volume.
///
/// Windows has no device number to compare, so the volume root is derived from the spelling.
/// `Unknown` never equals anything — an unrecognized root is treated as a different volume, which
/// costs a fallback rather than a failed rename.
#[cfg(windows)]
pub fn same_device(left: &Path, right: &Path) -> bool {
    match (
        win_root_of(&left.to_string_lossy()),
        win_root_of(&right.to_string_lossy()),
    ) {
        (WinRoot::Drive(left), WinRoot::Drive(right))
        | (WinRoot::Share(left), WinRoot::Share(right)) => left.eq_ignore_ascii_case(&right),
        _ => false,
    }
}

/// The volume root Windows' volume APIs want, derived from a root path.
///
/// `GetDriveTypeW` and `GetVolumeInformationW` both take a volume root with a trailing backslash
/// and nothing deeper. Kept pure so the spellings — UNC, extended-length, bare drive — are
/// testable without those volumes existing.
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
pub enum WinRoot {
    /// `D:\` — the drive type has to be asked for.
    Drive(String),
    /// `\\host\share\` — reached over the network by construction, no call needed.
    Share(String),
    Unknown,
}

#[cfg(any(windows, test))]
pub fn win_root_of(path: &str) -> WinRoot {
    let s = path.replace('/', "\\");
    // Extended-length prefixes bypass Win32 path parsing but name the same volumes.
    let s = if let Some(r) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{r}")
    } else if let Some(r) = s.strip_prefix(r"\\?\") {
        r.to_string()
    } else {
        s
    };
    if let Some(rest) = s.strip_prefix(r"\\") {
        let mut seg = rest.splitn(3, '\\');
        let (host, share) = (seg.next().unwrap_or(""), seg.next().unwrap_or(""));
        if host.is_empty() || share.is_empty() {
            return WinRoot::Unknown;
        }
        return WinRoot::Share(format!(r"\\{host}\{share}\"));
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return WinRoot::Drive(format!("{}:\\", (b[0] as char).to_ascii_uppercase()));
    }
    WinRoot::Unknown
}

/// The path spellings are checkable on any host, which is the point of keeping them pure — the
/// alternative is a UNC share and a spare drive letter to hand.
#[cfg(test)]
mod tests {
    use super::*;

    /// A path under a root is on that root's device. The lane asks this about a trash directory
    /// it may be about to create, so a not-yet-existing path must resolve through its parent.
    #[test]
    fn nested_roots_are_recognized_as_the_same_device() {
        let root = std::env::temp_dir().join(format!("syncdash-device-{}", std::process::id()));
        let nested = root.join("nested");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&nested).unwrap();
        assert!(same_device(&root, &nested));
        assert!(
            same_device(&root, &nested.join("not-created-yet")),
            "an absent path resolves through its nearest existing ancestor"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_unc_root_is_a_share_whatever_the_spelling() {
        assert_eq!(
            win_root_of(r"\\nas\photos\2026"),
            WinRoot::Share(r"\\nas\photos\".into())
        );
        assert_eq!(
            win_root_of(r"\\nas\photos"),
            WinRoot::Share(r"\\nas\photos\".into())
        );
        // The extended-length UNC spelling names the same share
        assert_eq!(
            win_root_of(r"\\?\UNC\nas\photos\sub"),
            WinRoot::Share(r"\\nas\photos\".into())
        );
        // A host with no share names no volume — better Unknown than a wrong guess
        assert_eq!(win_root_of(r"\\nas"), WinRoot::Unknown);
    }

    #[test]
    fn a_drive_root_survives_every_spelling() {
        assert_eq!(win_root_of(r"D:\Code\x"), WinRoot::Drive(r"D:\".into()));
        assert_eq!(win_root_of("D:/Code/x"), WinRoot::Drive(r"D:\".into()));
        assert_eq!(win_root_of("d:/code"), WinRoot::Drive(r"D:\".into()));
        assert_eq!(win_root_of("D:"), WinRoot::Drive(r"D:\".into()));
        // \\?\ is a parsing escape, not a different volume
        assert_eq!(
            win_root_of(r"\\?\D:\very\long"),
            WinRoot::Drive(r"D:\".into())
        );
        // A relative root names no volume
        assert_eq!(win_root_of("relative/dir"), WinRoot::Unknown);
    }
}
