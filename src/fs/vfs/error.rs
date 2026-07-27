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
        VfsError { kind, detail: detail.into(), source: None }
    }

    pub fn with_source(
        kind: VfsErrorKind,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> VfsError {
        VfsError { kind, detail: detail.into(), source: Some(Box::new(source)) }
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
        self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// io::Error -> VfsError, for backends that sit on std::fs (local, SMB translation).
/// For a **local** filesystem NotFound really is confirmed absence, so the mapping is direct.
impl From<std::io::Error> for VfsError {
    fn from(e: std::io::Error) -> VfsError {
        use std::io::ErrorKind as K;
        let kind = match e.kind() {
            K::NotFound => VfsErrorKind::NotFound,
            K::PermissionDenied => VfsErrorKind::PermissionDenied,
            K::AlreadyExists => VfsErrorKind::AlreadyExists,
            K::DirectoryNotEmpty => VfsErrorKind::NotEmpty,
            K::Interrupted => VfsErrorKind::Cancelled,
            K::TimedOut | K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe
            | K::ConnectionRefused | K::HostUnreachable | K::NetworkUnreachable
            | K::NetworkDown => VfsErrorKind::Transient,
            _ => VfsErrorKind::Io,
        };
        let detail = e.to_string();
        VfsError { kind, detail, source: Some(Box::new(e)) }
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
            VfsErrorKind::NotEmpty => K::DirectoryNotEmpty,
            VfsErrorKind::Cancelled => K::Interrupted,
            VfsErrorKind::Transient => K::TimedOut,
            VfsErrorKind::Unsupported => K::Unsupported,
            VfsErrorKind::Protocol | VfsErrorKind::Io => K::Other,
        };
        std::io::Error::new(kind, e)
    }
}

/// Recover the VfsError classification from an io::Error that carried one.
pub fn as_vfs_error(e: &std::io::Error) -> Option<&VfsError> {
    e.get_ref()?.downcast_ref::<VfsError>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_io_error() {
        let v = VfsError::new(VfsErrorKind::Transient, "link dropped");
        let io: std::io::Error = v.into();
        assert_eq!(io.kind(), std::io::ErrorKind::TimedOut);
        let back = as_vfs_error(&io).expect("payload survives");
        assert_eq!(back.kind, VfsErrorKind::Transient);
        assert_eq!(back.detail, "link dropped");
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
    fn local_not_found_is_confirmed_absence() {
        let v: VfsError = std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
        assert_eq!(v.kind, VfsErrorKind::NotFound);
    }
}
