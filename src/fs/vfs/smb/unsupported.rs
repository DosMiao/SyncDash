//! Everything else: decline, and say so in a sentence that names the two platforms that work.

use std::path::PathBuf;

use crate::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use crate::fs::vfs::spec::RemoteSpec;
use crate::fs::vfs::CredentialProvider;

pub fn resolve(
    spec: &RemoteSpec,
    _share: &str,
    _sub: &str,
    _creds: &dyn CredentialProvider,
) -> VfsResult<PathBuf> {
    Err(VfsError::new(
        VfsErrorKind::Unsupported,
        format!("smb translation is implemented for Windows and macOS only (root: {})", spec.display()),
    ))
}