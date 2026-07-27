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
        // Full local semantics — that is the entire point of the translation route.
        let mut c = LocalVfs::new(PathBuf::new()).caps();
        c.protocol = "smb";
        // The OS trash has no reach onto a network share; the honest preserve story
        // (in-root retention area) arrives with the VFS apply lane.
        c.local_trash = false;
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

#[cfg(windows)]
mod platform {
    use super::*;

    /// `smb://[user@]host/share/sub` → `\\host\share\sub`, attaching credentials over
    /// WNetAddConnection2W when the plain probe is refused. The password crosses into
    /// the API in memory only — never argv, never a temp file.
    pub fn resolve(
        spec: &RemoteSpec,
        share: &str,
        sub: &str,
        creds: &dyn CredentialProvider,
    ) -> VfsResult<PathBuf> {
        let unc_share = format!(r"\\{}\{}", spec.host, share);
        let full = if sub.is_empty() {
            PathBuf::from(&unc_share)
        } else {
            PathBuf::from(format!(r"{unc_share}\{}", crate::foundation::path::to_native(sub)))
        };

        match probe(&full) {
            Ok(()) => return Ok(full),
            Err(ProbeFail::Denied) => {}
            Err(ProbeFail::Hard(e)) => return Err(e),
        }

        // Access denied under the current login: this is the credentialed path.
        let Some(user) = spec.user.clone() else {
            return Err(VfsError::new(
                VfsErrorKind::Auth,
                format!(
                    "access to {unc_share} denied under the current Windows login — name a user in the phrase (smb://user@{}/{share}) and store its password with: syncdash cred set \"{}\"",
                    spec.host,
                    spec.display()
                ),
            ));
        };
        let c = creds.credentials_for(spec)?;
        let Some(password) = c.password else {
            return Err(VfsError::new(
                VfsErrorKind::Auth,
                format!(
                    "no stored password for {user}@{} — store it with: syncdash cred set \"{}\"",
                    spec.host,
                    spec.display()
                ),
            ));
        };

        // Plain username first; a device-local account often needs the HOST\user form,
        // so a logon failure retries once with that spelling. Both attempts are named
        // in the error if neither lands.
        let mut tried = Vec::new();
        for u in [user.clone(), format!(r"{}\{}", spec.host, user)] {
            if u.contains('\\') && user.contains('\\') && tried.len() == 1 {
                break; // the user already carried a domain; no second spelling to try
            }
            match wnet_connect(&unc_share, &u, &password) {
                Ok(()) => {
                    return match probe(&full) {
                        Ok(()) => Ok(full),
                        Err(ProbeFail::Denied) => Err(VfsError::new(
                            VfsErrorKind::Auth,
                            format!("{unc_share} accepted the session for '{u}' but still refuses '{}'", full.display()),
                        )),
                        Err(ProbeFail::Hard(e)) => Err(e),
                    };
                }
                Err(code @ 1326) | Err(code @ 86) => {
                    tried.push(format!("'{u}' (logon failure {code})"));
                    continue;
                }
                Err(1219) => {
                    return Err(VfsError::new(
                        VfsErrorKind::Protocol,
                        format!(
                            "Windows already holds a session to {} under a different account (error 1219) — drop it first: net use {unc_share} /delete",
                            spec.host
                        ),
                    ));
                }
                Err(code @ (53 | 1231 | 1232 | 121 | 1203)) => {
                    return Err(VfsError::new(
                        VfsErrorKind::Transient,
                        format!("cannot reach {} (WNet error {code})", spec.host),
                    ));
                }
                Err(67) => {
                    return Err(VfsError::new(
                        VfsErrorKind::Protocol,
                        format!("{} answered: no share named '{share}' (error 67)", spec.host),
                    ));
                }
                Err(code) => {
                    return Err(VfsError::new(
                        VfsErrorKind::Io,
                        format!("WNetAddConnection2 to {unc_share} failed with Windows error {code}"),
                    ));
                }
            }
        }
        Err(VfsError::new(
            VfsErrorKind::Auth,
            format!(
                "{unc_share} rejected the stored password for {} — update it with: syncdash cred set \"{}\"",
                tried.join(" and "),
                spec.display()
            ),
        ))
    }

    enum ProbeFail {
        /// Denied under the current session — worth trying credentials.
        Denied,
        Hard(VfsError),
    }

    fn probe(p: &std::path::Path) -> Result<(), ProbeFail> {
        match std::fs::metadata(p) {
            Ok(_) => Ok(()),
            Err(e) => match e.raw_os_error() {
                Some(5) | Some(1326) | Some(86) => Err(ProbeFail::Denied),
                Some(53) | Some(1231) | Some(1232) | Some(121) | Some(1203) => Err(ProbeFail::Hard(VfsError::new(
                    VfsErrorKind::Transient,
                    format!("cannot reach '{}': {e}", p.display()),
                ))),
                Some(67) => Err(ProbeFail::Hard(VfsError::new(
                    VfsErrorKind::Protocol,
                    format!("the server answered: no such share ('{}', error 67)", p.display()),
                ))),
                _ if e.kind() == std::io::ErrorKind::NotFound => Err(ProbeFail::Hard(VfsError::new(
                    VfsErrorKind::NotFound,
                    format!("the share is reachable but '{}' does not exist on it", p.display()),
                ))),
                _ => Err(ProbeFail::Hard(e.into())),
            },
        }
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    struct NETRESOURCEW {
        dwScope: u32,
        dwType: u32,
        dwDisplayType: u32,
        dwUsage: u32,
        lpLocalName: *mut u16,
        lpRemoteName: *mut u16,
        lpComment: *mut u16,
        lpProvider: *mut u16,
    }

    #[link(name = "mpr")]
    extern "system" {
        fn WNetAddConnection2W(
            lpNetResource: *mut NETRESOURCEW,
            lpPassword: *const u16,
            lpUserName: *const u16,
            dwFlags: u32,
        ) -> u32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Device-less session (no drive letter): exactly what `net use \\host\share` does,
    /// minus the password-in-argv exposure.
    fn wnet_connect(unc_share: &str, user: &str, password: &str) -> Result<(), u32> {
        const RESOURCETYPE_DISK: u32 = 1;
        let mut remote = wide(unc_share);
        let user_w = wide(user);
        let pass_w = wide(password);
        let mut nr = NETRESOURCEW {
            dwScope: 0,
            dwType: RESOURCETYPE_DISK,
            dwDisplayType: 0,
            dwUsage: 0,
            lpLocalName: std::ptr::null_mut(),
            lpRemoteName: remote.as_mut_ptr(),
            lpComment: std::ptr::null_mut(),
            lpProvider: std::ptr::null_mut(),
        };
        let code = unsafe { WNetAddConnection2W(&mut nr, pass_w.as_ptr(), user_w.as_ptr(), 0) };
        if code == 0 {
            Ok(())
        } else {
            Err(code)
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    /// Order of preference: an existing smbfs mount of this share (Finder's or ours) →
    /// `mount_smbfs -N` into a private mount point (password from the login Keychain —
    /// tick "remember password" on the first Finder connect) → `osascript mount volume`
    /// as the GUI-prompt fallback. The password from our own store is *not* handed to
    /// mount_smbfs: it would have to travel through argv, visible to every process.
    pub fn resolve(
        spec: &RemoteSpec,
        share: &str,
        sub: &str,
        _creds: &dyn CredentialProvider,
    ) -> VfsResult<PathBuf> {
        if let Some(mp) = find_existing_mount(&spec.host, share, spec.user.as_deref()) {
            return Ok(join_sub(&mp, sub));
        }

        // Private mount point, reused across runs, cleaned by `syncdash net umount`
        let mp = private_mount_dir().join(format!("{}-{}", spec.host.to_lowercase(), share.to_lowercase()));
        std::fs::create_dir_all(&mp)
            .map_err(|e| VfsError::new(VfsErrorKind::Io, format!("cannot create mount point '{}': {e}", mp.display())))?;

        let url = match &spec.user {
            Some(u) => format!("//{u}@{}/{share}", spec.host),
            None => format!("//{}/{share}", spec.host),
        };
        let out = std::process::Command::new("mount_smbfs").arg("-N").arg(&url).arg(&mp).output();
        match out {
            Ok(o) if o.status.success() => return Ok(join_sub(&mp, sub)),
            Ok(o) => {
                let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
                // Authentication failure → one GUI attempt via Finder's own dialog
                if msg.contains("Authentication error") || msg.contains("Permission denied") {
                    if osascript_mount(&spec.host, share).is_ok() {
                        if let Some(mp2) = find_existing_mount(&spec.host, share, spec.user.as_deref()) {
                            return Ok(join_sub(&mp2, sub));
                        }
                    }
                    return Err(VfsError::new(
                        VfsErrorKind::Auth,
                        format!(
                            "mount_smbfs was refused for {url} ({msg}) — connect once in Finder with 'remember password' ticked, then retry"
                        ),
                    ));
                }
                return Err(VfsError::new(
                    VfsErrorKind::Transient,
                    format!("mount_smbfs {url} failed: {msg}"),
                ));
            }
            Err(e) => {
                return Err(VfsError::new(VfsErrorKind::Io, format!("cannot run mount_smbfs: {e}")))
            }
        }
    }

    fn join_sub(mp: &std::path::Path, sub: &str) -> PathBuf {
        if sub.is_empty() {
            mp.to_path_buf()
        } else {
            crate::foundation::path::join_native(mp, sub)
        }
    }

    pub fn private_mount_dir() -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join("Library").join("Caches").join("syncdash").join("mnt")
    }

    /// Scan the mount table for an smbfs mount of //[user@]host…/share.
    fn find_existing_mount(host: &str, share: &str, want_user: Option<&str>) -> Option<PathBuf> {
        let out = std::process::Command::new("mount").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if !line.contains("(smbfs") {
                continue;
            }
            // "//ben@nas._smb._tcp.local/share on /Volumes/share (smbfs, ...)"
            let Some(rest) = line.strip_prefix("//") else { continue };
            let Some((source, after)) = rest.split_once(" on ") else { continue };
            let mountpoint = after.split(" (").next().unwrap_or("").trim();
            let (muser, hostshare) = match source.split_once('@') {
                Some((u, hs)) => (Some(u), hs),
                None => (None, source),
            };
            let Some((mhost, mshare)) = hostshare.split_once('/') else { continue };
            // Bonjour spelling: "nas._smb._tcp.local" — compare on the first label
            let mhost_base = mhost.split("._smb").next().unwrap_or(mhost);
            let host_base = host.split('.').next().unwrap_or(host);
            if !mhost_base.eq_ignore_ascii_case(host_base) && !mhost.eq_ignore_ascii_case(host) {
                continue;
            }
            if !mshare.eq_ignore_ascii_case(share) {
                continue;
            }
            if let (Some(want), Some(got)) = (want_user, muser) {
                if !got.eq_ignore_ascii_case(want) {
                    continue; // mounted, but as somebody else — not ours to claim
                }
            }
            if !mountpoint.is_empty() {
                return Some(PathBuf::from(mountpoint));
            }
        }
        None
    }

    fn osascript_mount(host: &str, share: &str) -> std::io::Result<()> {
        let status = std::process::Command::new("osascript")
            .arg("-e")
            .arg(format!("mount volume \"smb://{host}/{share}\""))
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::Other, "osascript mount refused"))
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::*;

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
}

/// `syncdash net umount`: release the private mount points this tool created.
/// macOS only — on Windows the sessions are device-less; `net use \\host\share /delete`
/// is the OS's own tool for dropping one.
pub fn umount_private_mounts() -> Vec<(String, Result<(), String>)> {
    #[cfg(target_os = "macos")]
    {
        let dir = platform::private_mount_dir();
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                let label = p.display().to_string();
                let r = std::process::Command::new("umount").arg(&p).status();
                match r {
                    Ok(s) if s.success() => out.push((label, Ok(()))),
                    Ok(s) => out.push((label, Err(format!("umount exited with {s}")))),
                    Err(err) => out.push((label, Err(err.to_string()))),
                }
            }
        }
        out
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
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
