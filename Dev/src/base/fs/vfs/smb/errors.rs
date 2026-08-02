//! Mapping SMB protocol failures onto the VFS error taxonomy.
//!
//! The asymmetry is the point: everything ambiguous becomes Transient, and only a server's own
//! definite "no such name" becomes NotFound — which callers still confirm against a parent listing
//! before trusting. A protocol hiccup read as a deletion is the failure this tool exists not to
//! have.

use super::super::error::{VfsError, VfsErrorKind};
use smb2::types::status::NtStatus;
use smb2::types::Command;

/// `STATUS_DIRECTORY_NOT_EMPTY`. The crate's NtStatus table has no constant for it and its
/// `ErrorKind` folds it into `Other`, but the engine's whole delete-dir classification rides
/// on telling this one apart from a generic protocol error, so it is matched on the raw code.
pub(super) const STATUS_DIRECTORY_NOT_EMPTY: u32 = 0xC000_0101;

/// Map the crate's errors onto the VFS taxonomy.
///
/// The asymmetry in `error.rs` is the point: everything ambiguous goes to `Transient`, and
/// only a server's own definite "no such name" becomes `NotFound` — which callers still
/// double-check before trusting.
pub(super) fn map_smb_err(what: &str, e: smb2::Error) -> VfsError {
    use smb2::ErrorKind as K;
    if let smb2::Error::Protocol { status, .. } = &e {
        if status.0 == STATUS_DIRECTORY_NOT_EMPTY {
            return VfsError::new(
                VfsErrorKind::NotEmpty,
                format!("{what}: the directory is not empty"),
            );
        }
    }
    let kind = match e.kind() {
        K::NotFound => VfsErrorKind::NotFound,
        K::AlreadyExists => VfsErrorKind::AlreadyExists,
        K::AccessDenied => VfsErrorKind::PermissionDenied,
        K::AuthRequired | K::SigningRequired => VfsErrorKind::Auth,
        K::Cancelled => VfsErrorKind::Cancelled,
        // A held file, a dropped link and an expired session all clear on a retry, and none
        // of them says anything about whether the file exists.
        K::SharingViolation | K::ConnectionLost | K::TimedOut | K::SessionExpired => {
            VfsErrorKind::Transient
        }
        K::Io => VfsErrorKind::Io,
        // The server answered with a code — evidence the connection itself is fine.
        _ => VfsErrorKind::Protocol,
    };
    VfsError::new(kind, format!("{what}: {e}"))
}

/// A non-success status from one sub-response of a compound, routed through the same table.
pub(super) fn status_err(what: &str, command: Command, status: NtStatus) -> VfsError {
    map_smb_err(what, smb2::Error::Protocol { status, command })
}
