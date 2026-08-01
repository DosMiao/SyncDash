//! VFS error taxonomy. The load-bearing distinction is `Transient` vs `NotFound`:
//! a dropped connection reported as "file gone" turns into a reverse delete on the
//! next compare — the one failure mode this tool exists to never have. Backends must
//! only return `NotFound` after confirming absence (for protocols where a missing-file
//! error is ambiguous, that means listing the parent and checking the name is really gone).

use std::fmt;

pub type VfsResult<T> = Result<T, VfsError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VfsErrorKind {
    /// Confirmed absent. Never a guess: connection trouble must go to `Transient`.
    NotFound,
    /// Network / timeout / connection-layer failure. Retryable; must never be
    /// mistaken for `NotFound` (that asymmetry is deliberate: reporting a deleted
    /// file as a connection error is an annoyance, the reverse is data loss).
    Transient,
    /// Credentials missing or rejected. Hard stop — the engine never continues
    /// anonymously on a root that asked for auth. Detail should name the remedy
    /// (`syncdash cred set "<phrase>"`).
    Auth,
    PermissionDenied,
    /// The backend does not have this capability. Normal flows are routed around
    /// this at preflight via `caps()`; seeing it at run time means the capability
    /// map has a hole and it gets logged at error level.
    Unsupported,
    /// remove_dir on a non-empty directory (the engine classifies delete-dir outcomes on this).
    NotEmpty,
    /// rename refused because the destination exists (SFTP v3 semantics).
    AlreadyExists,
    /// A rename crossed filesystem/volume boundaries. This is the only rename failure for which
    /// apply may safely select its transactional copy fallback.
    CrossDevice,
    /// The server answered with a protocol-level error code — evidence that the
    /// connection itself is fine (the opposite signal of `Transient`).
    Protocol,
    /// Cooperative cancel. Maps to io::ErrorKind::Interrupted both ways so the
    /// engine's existing cancel plumbing keeps working unchanged.
    Cancelled,
    /// Local OS error that fits none of the above.
    Io,
}

#[derive(Debug)]
pub struct VfsError {
    pub kind: VfsErrorKind,
    /// Human-readable context: protocol, host, path, the server's own words.
    pub detail: String,
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl VfsError {
    pub fn new(kind: VfsErrorKind, detail: impl Into<String>) -> VfsError {
        VfsError {
            kind,
            detail: detail.into(),
            source: None,
        }
    }

    pub fn unsupported(what: impl Into<String>) -> VfsError {
        VfsError::new(VfsErrorKind::Unsupported, what)
    }
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for VfsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// io::Error -> VfsError, for the local backend and anything else sitting on std::fs.
/// For a **local** filesystem NotFound really is confirmed absence, so the mapping is direct.
impl From<std::io::Error> for VfsError {
    fn from(e: std::io::Error) -> VfsError {
        use std::io::ErrorKind as K;
        // Windows raw codes that std leaves as `Uncategorized` — measured, not assumed:
        // a file held with share_mode(0) (what Word, Excel, Outlook and a running .exe do)
        // fails delete, rename *and* read with kind()==Uncategorized, raw 32. Left in the
        // default arm it lands in `Io`, i.e. "a real local failure"; it is in fact the most
        // common transient condition on Windows and succeeds on the next run. 1314 is the
        // symlink privilege, which is not transient but is a specific, actionable refusal.
        #[cfg(windows)]
        if let Some((kind, hint)) = match e.raw_os_error() {
            Some(17) => Some((VfsErrorKind::CrossDevice, "source and destination are on different volumes")),
            Some(32) => Some((VfsErrorKind::Transient, "file is open in another program (sharing violation); it will be retried on the next run")),
            Some(33) => Some((VfsErrorKind::Transient, "a byte range of the file is locked by another program; it will be retried on the next run")),
            Some(1314) => Some((VfsErrorKind::PermissionDenied, "creating symlinks needs Windows Developer Mode or an elevated process (SeCreateSymbolicLinkPrivilege)")),
            _ => None,
        } {
            return VfsError { kind, detail: format!("{e} — {hint}"), source: Some(Box::new(e)) };
        }
        #[cfg(unix)]
        if e.raw_os_error() == Some(libc::EXDEV) {
            return VfsError {
                kind: VfsErrorKind::CrossDevice,
                detail: e.to_string(),
                source: Some(Box::new(e)),
            };
        }
        #[cfg(unix)]
        if e.raw_os_error().is_some_and(|raw| {
            raw == libc::ENOTSUP || raw == libc::EOPNOTSUPP || raw == libc::ENOSYS
        }) {
            return VfsError {
                kind: VfsErrorKind::Unsupported,
                detail: e.to_string(),
                source: Some(Box::new(e)),
            };
        }
        // The Darwin counterpart of the block above: errnos std does not give a distinct
        // ErrorKind, where the difference is actionable.
        //
        // **EPERM is deliberately not classified here.** It is the errno a TCC denial produces —
        // ~/Desktop, ~/Documents, an external volume — but it is equally the errno for chmod of a
        // file you do not own, a sticky directory, and a `chflags uchg` file. Asserting "grant Full
        // Disk Access" on every EPERM would be a guess, and a wrong one often enough to send the
        // user after a checkbox that is not the problem. It stays PermissionDenied via the shared
        // match, with Privacy & Security offered as *one* possibility in the wording.
        #[cfg(target_os = "macos")]
        if let Some((kind, hint)) = match e.raw_os_error() {
            // ESTALE: a /Volumes share that dropped and remounted. The handle is dead, the volume
            // is not — retrying is the correct response, and classifying it Io meant a network
            // blip was reported as a permanent local failure.
            Some(70) => Some((VfsErrorKind::Transient, "the network volume was remounted and this handle is stale; it will be retried on the next run")),
            // EPERM, only where the shared match would already say PermissionDenied — this adds
            // the possibilities, it does not claim one.
            Some(1) => Some((VfsErrorKind::PermissionDenied, "not permitted: the file may be locked (chflags uchg), owned by another user, or the app may need Full Disk Access in System Settings > Privacy & Security")),
            _ => None,
        } {
            return VfsError { kind, detail: format!("{e} — {hint}"), source: Some(Box::new(e)) };
        }
        let kind = match e.kind() {
            K::NotFound => VfsErrorKind::NotFound,
            K::PermissionDenied => VfsErrorKind::PermissionDenied,
            K::AlreadyExists => VfsErrorKind::AlreadyExists,
            K::CrossesDevices => VfsErrorKind::CrossDevice,
            K::Unsupported => VfsErrorKind::Unsupported,
            K::DirectoryNotEmpty => VfsErrorKind::NotEmpty,
            K::Interrupted => VfsErrorKind::Cancelled,
            // A volume mounted read-only (an NTFS external, a DMG, a Time Machine snapshot) is a
            // permission problem, not an unclassified I/O one — every op against it will fail the
            // same way, so it must read as a refusal rather than as bad luck.
            K::ReadOnlyFilesystem => VfsErrorKind::PermissionDenied,
            // A stale handle is the portable spelling of the ESTALE case above.
            K::StaleNetworkFileHandle => VfsErrorKind::Transient,
            K::TimedOut
            | K::ConnectionReset
            | K::ConnectionAborted
            | K::BrokenPipe
            | K::ConnectionRefused
            | K::HostUnreachable
            | K::NetworkUnreachable
            | K::NetworkDown => VfsErrorKind::Transient,
            // StorageFull and QuotaExceeded stay Io on purpose: they are real, permanent, local
            // failures. CrossesDevices is classified above because Move has one safe fallback.
            _ => VfsErrorKind::Io,
        };
        let detail = e.to_string();
        VfsError {
            kind,
            detail,
            source: Some(Box::new(e)),
        }
    }
}

/// VfsError -> io::Error, so `?` feeds straight into the engine's existing io::Result
/// plumbing. The VfsError rides along as the payload; `as_vfs_error` gets it back out.
impl From<VfsError> for std::io::Error {
    fn from(e: VfsError) -> std::io::Error {
        use std::io::ErrorKind as K;
        let kind = match e.kind {
            VfsErrorKind::NotFound => K::NotFound,
            VfsErrorKind::PermissionDenied | VfsErrorKind::Auth => K::PermissionDenied,
            VfsErrorKind::AlreadyExists => K::AlreadyExists,
            VfsErrorKind::CrossDevice => K::CrossesDevices,
            VfsErrorKind::NotEmpty => K::DirectoryNotEmpty,
            VfsErrorKind::Cancelled => K::Interrupted,
            VfsErrorKind::Transient => K::TimedOut,
            VfsErrorKind::Unsupported => K::Unsupported,
            VfsErrorKind::Protocol | VfsErrorKind::Io => K::Other,
        };
        std::io::Error::new(kind, e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_io_error() {
        let v = VfsError::new(VfsErrorKind::Transient, "link dropped");
        let io: std::io::Error = v.into();
        assert_eq!(io.kind(), std::io::ErrorKind::TimedOut);
        // The classification is what survives the crossing — `From` re-derives it from
        // ErrorKind, so a Transient that becomes an io::Error comes back Transient.
        let back: VfsError = io.into();
        assert_eq!(back.kind, VfsErrorKind::Transient);
        assert!(back.detail.contains("link dropped"), "{}", back.detail);
    }

    #[test]
    fn cancel_maps_to_interrupted_both_ways() {
        let v = VfsError::new(VfsErrorKind::Cancelled, "stop");
        let io: std::io::Error = v.into();
        assert_eq!(io.kind(), std::io::ErrorKind::Interrupted);
        let v2: VfsError = std::io::Error::new(std::io::ErrorKind::Interrupted, "stop").into();
        assert_eq!(v2.kind, VfsErrorKind::Cancelled);
    }

    #[test]
    fn cross_device_maps_both_ways_without_becoming_a_generic_io_error() {
        let v: VfsError = std::io::Error::from(std::io::ErrorKind::CrossesDevices).into();
        assert_eq!(v.kind, VfsErrorKind::CrossDevice);

        let io: std::io::Error =
            VfsError::new(VfsErrorKind::CrossDevice, "different volume").into();
        assert_eq!(io.kind(), std::io::ErrorKind::CrossesDevices);
        let roundtrip: VfsError = io.into();
        assert_eq!(roundtrip.kind, VfsErrorKind::CrossDevice);

        #[cfg(unix)]
        {
            let raw: VfsError = std::io::Error::from_raw_os_error(libc::EXDEV).into();
            assert_eq!(raw.kind, VfsErrorKind::CrossDevice);
        }
    }

    #[test]
    fn unsupported_os_operations_fail_closed_as_unsupported() {
        let v: VfsError = std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic no-replace unavailable",
        )
        .into();
        assert_eq!(v.kind, VfsErrorKind::Unsupported);

        #[cfg(unix)]
        {
            let raw: VfsError = std::io::Error::from_raw_os_error(libc::ENOTSUP).into();
            assert_eq!(raw.kind, VfsErrorKind::Unsupported);
        }
    }

    /// A `/Volumes` share that dropped and remounted mid-run leaves stale handles. That is the
    /// classic macOS sync interruption and it is recoverable, so it must not be reported as a
    /// permanent local failure the way the unclassified `Io` bucket would.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_stale_handle_from_a_remounted_share_is_transient() {
        let v: VfsError = std::io::Error::from_raw_os_error(70).into(); // ESTALE, per sys/errno.h
        assert_eq!(v.kind, VfsErrorKind::Transient);
        assert!(v.detail.contains("remounted"), "{}", v.detail);
    }

    /// EPERM must stay PermissionDenied and must **not** assert a cause. It is what a TCC denial
    /// produces, and equally what a chflags-locked file, a sticky directory and a foreign-owned
    /// file produce — naming Full Disk Access as the answer would send the user after the wrong
    /// checkbox most of the time. The wording offers the possibilities; the classification does
    /// not pick one.
    #[cfg(target_os = "macos")]
    #[test]
    fn eperm_offers_causes_without_claiming_one() {
        let v: VfsError = std::io::Error::from_raw_os_error(1).into();
        assert_eq!(v.kind, VfsErrorKind::PermissionDenied);
        assert!(
            v.detail.contains("Full Disk Access"),
            "the remedy has to be reachable: {}",
            v.detail
        );
        assert!(
            v.detail.contains("may"),
            "but it must be offered, not asserted: {}",
            v.detail
        );
    }

    /// A read-only volume (an NTFS external, a mounted DMG, a Time Machine snapshot) fails every
    /// op the same way. That is a refusal, not bad luck.
    #[test]
    fn a_read_only_volume_reads_as_a_refusal() {
        let v: VfsError = std::io::Error::from(std::io::ErrorKind::ReadOnlyFilesystem).into();
        assert_eq!(v.kind, VfsErrorKind::PermissionDenied);
    }

    /// The single most common Windows sync failure: an open Office document / running binary.
    /// std hands it back as `Uncategorized`, so only the raw code identifies it.
    #[cfg(windows)]
    #[test]
    fn a_file_held_open_by_another_program_is_transient_not_io() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = std::env::temp_dir().join(format!("syncdash-share-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("locked.txt");
        std::fs::write(&p, b"data").unwrap();
        let _hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&p)
            .unwrap();

        let io = std::fs::remove_file(&p).expect_err("an exclusively-held file cannot be deleted");
        assert_eq!(
            io.raw_os_error(),
            Some(32),
            "premise: ERROR_SHARING_VIOLATION"
        );
        assert_ne!(
            io.kind(),
            std::io::ErrorKind::PermissionDenied,
            "premise: std does not classify it"
        );
        let v: VfsError = io.into();
        assert_eq!(v.kind, VfsErrorKind::Transient);
        assert!(v.detail.contains("another program"), "{}", v.detail);

        drop(_hold);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn local_not_found_is_confirmed_absence() {
        let v: VfsError = std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
        assert_eq!(v.kind, VfsErrorKind::NotFound);
    }
}
