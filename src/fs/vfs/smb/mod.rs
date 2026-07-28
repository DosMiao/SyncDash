//! The SMB backend, translation flavor: an `smb://` phrase resolves to a path the
//! OS's own SMB client serves — UNC on Windows, an smbfs mount point on macOS — and
//! everything else delegates to `LocalVfs` on that path. `as_local()` then routes the
//! whole engine down the existing fast lanes, inheriting every already-fixed local
//! behavior (atomic staging, mtime correction, mmap hashing) for free.
//!
//! Division of labor against plain UNC (which keeps working untouched):
//! `\\server\share` = "use my current login, mounting is my problem";
//! `smb://user@server/share` = "SyncDash owns credentials and mount orchestration,
//! and says out loud what it did". Every connect step lands in the error detail or
//! the log — a share that silently half-works is how empty-directory disasters start.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::local::LocalVfs;
use super::spec::RemoteSpec;
use super::{
    CredentialProvider, ReadStream, VDirEntry, VMeta, Vfs, VfsCaps, WriteHint, WriteStaged,
};

mod platform;

pub use platform::umount_private_mounts;

pub struct SmbBackend {
    spec: RemoteSpec,
    creds: Arc<dyn CredentialProvider>,
    /// share name (first segment of the phrase root) and the path below it
    share: String,
    sub: String,
    /// Filled by connect(): the local path the OS serves this root at.
    inner: OnceLock<LocalVfs>,
    connect_gate: Mutex<()>,
}
impl SmbBackend {
    pub fn new(spec: RemoteSpec, creds: Arc<dyn CredentialProvider>) -> VfsResult<SmbBackend> {
        let mut segs = spec.root.splitn(2, '/');
        let share = segs.next().unwrap_or("").to_string();
        if share.is_empty() {
            return Err(VfsError::new(
                VfsErrorKind::Protocol,
                format!("'{}' names no share — an smb root needs at least smb://host/share", spec.display()),
            ));
        }
        let sub = segs.next().unwrap_or("").to_string();
        Ok(SmbBackend { spec, creds, share, sub, inner: OnceLock::new(), connect_gate: Mutex::new(()) })
    }

    fn inner(&self) -> VfsResult<&LocalVfs> {
        self.inner.get().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Transient,
                format!("'{}' is not connected — connect() must run first", self.spec.display()),
            )
        })
    }
}
impl Vfs for SmbBackend {
    fn caps(&self) -> VfsCaps {
        // Full local semantics — that is the entire point of the translation route — but read off
        // the *translated path* rather than a blank template. The volume probe there sees the
        // share for what it is, so `local_trash = false` and the narrower stream ceiling follow
        // from the medium instead of being patched back by hand afterwards (which is how the
        // field came to be set correctly and read by nobody).
        let mut c = match self.inner.get() {
            Some(l) => l.caps(),
            None => LocalVfs::new(PathBuf::new()).caps(),
        };
        c.protocol = "smb";
        c
    }

    fn display(&self) -> String {
        self.spec.display()
    }

    fn identity(&self) -> String {
        self.spec.identity()
    }

    fn as_local(&self) -> Option<&std::path::Path> {
        self.inner.get().and_then(|l| l.as_local())
    }

    fn server_info(&self) -> Option<String> {
        self.inner.get().and_then(|l| l.as_local()).map(|p| format!("os-translated: {}", p.display()))
    }

    fn connect(&self) -> VfsResult<()> {
        let _g = self.connect_gate.lock().unwrap();
        if self.inner.get().is_some() {
            return Ok(());
        }
        let root = platform::resolve(&self.spec, &self.share, &self.sub, self.creds.as_ref())?;
        let md = std::fs::metadata(&root).map_err(|e| {
            VfsError::new(
                VfsErrorKind::Transient,
                format!("'{}' resolved to '{}' but it is not readable: {e}", self.spec.display(), root.display()),
            )
        })?;
        if !md.is_dir() {
            return Err(VfsError::new(
                VfsErrorKind::Protocol,
                format!("'{}' resolved to '{}', which is not a directory", self.spec.display(), root.display()),
            ));
        }
        let _ = self.inner.set(LocalVfs::new(root));
        Ok(())
    }

    // ---- everything below is plain delegation onto the translated path ----

    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        self.inner()?.stat(rel)
    }
    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        self.inner()?.read_dir(rel)
    }
    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        self.inner()?.open_read(rel)
    }
    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        self.inner()?.read_range(rel, off, len)
    }
    fn read_link(&self, rel: &str) -> VfsResult<String> {
        self.inner()?.read_link(rel)
    }
    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        self.inner()?.mkdir_all(rel)
    }
    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        self.inner()?.open_write(rel, hint)
    }
    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        self.inner()?.rename(from_rel, to_rel)
    }
    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        self.inner()?.remove_file(rel)
    }
    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        self.inner()?.remove_dir(rel)
    }
    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        self.inner()?.set_mtime(rel, mtime_ms)
    }
    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        self.inner()?.set_mode(rel, mode)
    }
    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        self.inner()?.make_symlink(rel, target)
    }
    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        self.inner()?.free_space()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::spec::{parse, RootSpec};

    fn backend(s: &str) -> SmbBackend {
        let RootSpec::Remote(r) = parse(s) else { panic!() };
        SmbBackend::new(r, crate::fs::vfs::cred::default_provider()).unwrap()
    }

    #[test]
    fn share_and_sub_split() {
        let b = backend("smb://ben@server/photos/2026/07");
        assert_eq!(b.share, "photos");
        assert_eq!(b.sub, "2026/07");
        let b2 = backend("smb://server/backup");
        assert_eq!(b2.share, "backup");
        assert_eq!(b2.sub, "");
    }

    #[test]
    fn no_share_is_refused_at_construction() {
        let RootSpec::Remote(r) = parse("smb://server") else { panic!() };
        assert!(SmbBackend::new(r, crate::fs::vfs::cred::default_provider()).is_err());
    }

    #[test]
    fn unconnected_backend_says_so() {
        let b = backend("smb://server/share");
        let e = b.stat("x").unwrap_err();
        assert_eq!(e.kind, crate::fs::vfs::VfsErrorKind::Transient);
        assert!(b.as_local().is_none());
    }

    #[test]
    fn display_and_identity_stay_phrase_shaped() {
        let b = backend("smb://ben@Server/share/sub");
        assert_eq!(b.display(), "smb://ben@Server/share/sub");
        assert_eq!(b.identity(), "smb://ben@server:445/share/sub");
    }
}
