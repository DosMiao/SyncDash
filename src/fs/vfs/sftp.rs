//! The SFTP backend — pure-Rust russh + russh-sftp behind the sync `Vfs` trait.
//!
//! Why russh and not libssh2 bindings: this machine's users hold OpenSSH-format keys
//! (the only format ssh-keygen has emitted since 7.8), which libssh2's WinCNG backend
//! cannot read, and building its OpenSSL alternative needs a perl toolchain Windows
//! boxes don't have. russh reads those keys natively and pulls zero C dependencies
//! (ring backend). The FFS survey's *protocol*-level lessons still apply — SFTP v3
//! rename refuses existing targets, setstat-by-path after close, no statvfs — they
//! were never libssh2-specific.
//!
//! Concurrency model: one SSH session, one SFTP channel, requests multiplexed by the
//! protocol's own ids (russh-sftp is fully concurrent). The tokio runtime is private
//! to this backend — two worker threads, entered only through `block_on` from engine
//! threads; the trait stays sync and the engine never sees an async type.
//!
//! Read half only for now: every write-side method reports `Unsupported` naming the
//! milestone. The write lane (staged uploads, rename-into-place, setstat) lands next.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::spec::RemoteSpec;
use super::{
    CaseSense, CommitReport, CredentialProvider, Credentials, EntryKind, ReadStream, Support,
    VDirEntry, VMeta, Vfs, VfsCaps, WriteHint, WriteStaged,
};

use russh::keys::{known_hosts, load_secret_key, HashAlg, PrivateKeyWithHashAlg};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, StatusCode};

pub struct SftpBackend {
    spec: RemoteSpec,
    creds: Arc<dyn CredentialProvider>,
    rt: tokio::runtime::Runtime,
    timeout: Duration,
    conn: OnceLock<Conn>,
    connect_gate: Mutex<()>,
    server_line: Mutex<Option<String>>,
}

struct Conn {
    /// Kept alive for the session's sake; the sftp channel is what we talk to.
    _session: russh::client::Handle<HostCheck>,
    sftp: Arc<SftpSession>,
    /// Absolute server-side root ('/'-separated, no trailing '/').
    root_abs: String,
}

impl SftpBackend {
    pub fn new(spec: RemoteSpec, creds: Arc<dyn CredentialProvider>) -> SftpBackend {
        let timeout = spec
            .opt("timeout")
            .and_then(|t| t.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(20));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_io()
            .enable_time()
            .thread_name("syncdash-sftp")
            .build()
            .expect("tokio runtime");
        SftpBackend {
            spec,
            creds,
            rt,
            timeout,
            conn: OnceLock::new(),
            connect_gate: Mutex::new(()),
            server_line: Mutex::new(None),
        }
    }

    fn conn(&self) -> VfsResult<&Conn> {
        self.conn.get().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Transient,
                format!("'{}' is not connected — connect() must run first", self.spec.display()),
            )
        })
    }

    fn abs(&self, rel: &str) -> String {
        let root = &self.conn.get().expect("connected").root_abs;
        if rel.is_empty() {
            root.clone()
        } else {
            format!("{root}/{rel}")
        }
    }

    /// Run one sftp operation with the backend's timeout. A timeout is connection
    /// trouble, i.e. `Transient` — never anything that could read as "file gone".
    fn block<F, T>(&self, what: &str, fut: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = Result<T, russh_sftp::client::error::Error>>,
    {
        // The timeout future must be BUILT inside the runtime (it grabs the timer at
        // construction), hence the async block rather than a bare block_on(timeout(..)).
        let d = self.timeout;
        match self.rt.block_on(async { tokio::time::timeout(d, fut).await }) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(map_sftp_err(what, e)),
            Err(_) => Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("{what} timed out after {:?} on '{}'", self.timeout, self.spec.display()),
            )),
        }
    }

    /// The FFS defense, verbatim: a NoSuchFile answer is only *confirmed* absence
    /// after the parent directory has been listed and really does not carry the name.
    /// A listing that still shows it — or that cannot be obtained — is `Transient`.
    fn confirm_absent(&self, rel: &str) -> VfsResult<()> {
        let (parent, name) = crate::foundation::path::split_parent(rel);
        let parent = parent.trim_end_matches('/');
        let listing = {
            let sftp = self.conn()?.sftp.clone();
            let p = self.abs(parent);
            self.block("read_dir (absence check)", async move { sftp.read_dir(p).await })
        };
        match listing {
            Ok(dir) => {
                for e in dir {
                    if e.file_name() == name {
                        return Err(VfsError::new(
                            VfsErrorKind::Transient,
                            format!(
                                "the server reported '{rel}' missing but its parent still lists it — treating as a temporary fault, not a deletion"
                            ),
                        ));
                    }
                }
                Ok(())
            }
            Err(e) if e.kind == VfsErrorKind::NotFound => Ok(()), // parent gone too: absence stands
            Err(e) => Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("cannot confirm '{rel}' is really absent (parent listing failed: {e})"),
            )),
        }
    }

}

/// An attributes packet that asserts NOTHING. Never use `FileAttributes::default()`
/// for setstat: that crate's Default is `size: Some(0), uid/gid: Some(0),
/// permissions: Some(0o777|DIR), mtime: Some(0)` — sent as-is it TRUNCATES the file
/// to zero and clobbers its owner and mode. Found the hard way: a live apply landed
/// every file at 0 bytes after a "successful" rename.
fn attrs_none() -> FileAttributes {
    FileAttributes {
        size: None,
        uid: None,
        user: None,
        gid: None,
        group: None,
        permissions: None,
        atime: None,
        mtime: None,
    }
}

fn meta_of(a: &FileAttributes) -> VMeta {
    let kind = if a.is_dir() {
        EntryKind::Dir
    } else if a.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    VMeta {
        kind,
        size: a.size.unwrap_or(0),
        // SFTP v3 carries 32-bit whole seconds — precision 1000 ms, declared in caps
        mtime_ms: a.mtime.map(|s| s as i64 * 1000).unwrap_or(0),
        mode: a.permissions.map(|p| p & 0o7777),
        file_id: None,
        link: None,
    }
}

fn map_sftp_err(what: &str, e: russh_sftp::client::error::Error) -> VfsError {
    use russh_sftp::client::error::Error as E;
    match e {
        E::Status(s) => {
            let kind = match s.status_code {
                StatusCode::NoSuchFile => VfsErrorKind::NotFound, // callers double-check before trusting this
                StatusCode::PermissionDenied => VfsErrorKind::PermissionDenied,
                _ => VfsErrorKind::Protocol,
            };
            VfsError::new(kind, format!("{what}: server answered {}: {}", s.status_code, s.error_message))
        }
        E::Timeout => VfsError::new(VfsErrorKind::Transient, format!("{what}: sftp request timed out")),
        other => VfsError::new(VfsErrorKind::Transient, format!("{what}: {other}")),
    }
}

// ---- host-key verification ----

#[derive(Debug)]
enum HostCheckError {
    Ssh(russh::Error),
    Unknown { fingerprint: String },
    Changed { fingerprint: String },
}

impl std::fmt::Display for HostCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostCheckError::Ssh(e) => write!(f, "{e}"),
            HostCheckError::Unknown { fingerprint } => write!(f, "unknown host key {fingerprint}"),
            HostCheckError::Changed { fingerprint } => write!(f, "HOST KEY CHANGED to {fingerprint}"),
        }
    }
}
impl std::error::Error for HostCheckError {}
impl From<russh::Error> for HostCheckError {
    fn from(e: russh::Error) -> Self {
        HostCheckError::Ssh(e)
    }
}

/// Verifies against the user's own OpenSSH `~/.ssh/known_hosts` — the trust the ssh
/// smart-peer mode already built up is reused, not duplicated. Unknown host = refuse
/// with the fingerprint and the remedy (connect once with ssh); changed key = refuse
/// loudly, never auto-continue.
struct HostCheck {
    host: String,
    port: u16,
}

impl russh::client::Handler for HostCheck {
    type Error = HostCheckError;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        match known_hosts::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => Ok(true),
            Ok(false) => Err(HostCheckError::Unknown { fingerprint: fp }),
            Err(_) => Err(HostCheckError::Changed { fingerprint: fp }),
        }
    }
}

impl Vfs for SftpBackend {
    fn caps(&self) -> VfsCaps {
        VfsCaps {
            protocol: "sftp",
            mtime_precision_ms: 1000, // v3 attributes carry whole seconds
            set_mtime: Support::Yes,  // setstat-by-path (write lane)
            fsync: Support::Unknown,  // fsync@openssh.com probed when the write lane lands
            rename: Support::Yes,
            rename_overwrite: Support::No, // v3 rename refuses an existing target (relied upon)
            ranged_read: Support::Yes,     // seekable file handles → real sampled evidence
            write_at: Support::No,         // revisited with the write lane
            unix_mode: Support::Yes,
            symlink: Support::Yes,
            file_id: Support::No,
            free_space: Support::No, // statvfs corrupts sessions on some servers (FFS finding); never asked
            read_back: Support::Yes,
            local_trash: false,
            case_sensitivity: CaseSense::Unknown,
            max_parallel_streams: 4,
        }
    }

    fn display(&self) -> String {
        self.spec.display()
    }

    fn identity(&self) -> String {
        self.spec.identity()
    }

    fn server_info(&self) -> Option<String> {
        self.server_line.lock().unwrap().clone()
    }

    fn connect(&self) -> VfsResult<()> {
        let _g = self.connect_gate.lock().unwrap();
        if self.conn.get().is_some() {
            return Ok(());
        }
        let host = self.spec.host.clone();
        let port = self.spec.port.unwrap_or(22);
        let user = self.spec.user.clone().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Auth,
                format!("'{}' names no user — spell it sftp://user@host/…", self.spec.display()),
            )
        })?;
        let creds = self.creds.credentials_for(&self.spec)?;
        let timeout = self.timeout;
        let root_spec = self.spec.root.clone();
        let display = self.spec.display();

        let built = self.rt.block_on(async {
            connect_and_open(&host, port, &user, &creds, timeout, &root_spec, &display).await
        })?;
        *self.server_line.lock().unwrap() = Some(built.3);
        let _ = self.conn.set(Conn { _session: built.0, sftp: Arc::new(built.1), root_abs: built.2 });
        Ok(())
    }

    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        match self.block("stat", async move { sftp.symlink_metadata(p).await }) {
            Ok(a) => Ok(Some(meta_of(&a))),
            Err(e) if e.kind == VfsErrorKind::NotFound => {
                if rel.is_empty() {
                    return Ok(None); // the root itself missing needs no parent check
                }
                self.confirm_absent(rel)?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        let dir = self.block("read_dir", async move { sftp.read_dir(p).await })?;
        let mut out = Vec::new();
        for e in dir {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            out.push(VDirEntry { name, meta: meta_of(&e.metadata()) });
        }
        Ok(out)
    }

    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        let file = self.block("open", async move { sftp.open(p).await })?;
        Ok(Box::new(SftpRead {
            rt: self.rt.handle().clone(),
            timeout: self.timeout,
            file,
        }))
    }

    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        let timeout = self.timeout;
        let res = self.rt.block_on(async move { tokio::time::timeout(timeout, async move {
            let mut f = sftp.open(p).await?;
            f.seek(std::io::SeekFrom::Start(off)).await.map_err(russh_sftp::client::error::Error::from)?;
            let mut buf = vec![0u8; len as usize];
            let mut got = 0usize;
            while got < buf.len() {
                let n = f
                    .read(&mut buf[got..])
                    .await
                    .map_err(russh_sftp::client::error::Error::from)?;
                if n == 0 {
                    break; // short only at EOF, per contract
                }
                got += n;
            }
            buf.truncate(got);
            Ok::<_, russh_sftp::client::error::Error>(buf)
        }).await });
        match res {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(map_sftp_err("read_range", e)),
            Err(_) => Err(VfsError::new(VfsErrorKind::Transient, "read_range timed out")),
        }
    }

    fn read_link(&self, rel: &str) -> VfsResult<String> {
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        self.block("read_link", async move { sftp.read_link(p).await })
    }

    // ---- write side ----

    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        let sftp = self.conn()?.sftp.clone();
        let mut prefix = String::new();
        for seg in rel.split('/').filter(|s| !s.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
            let abs = self.abs(&prefix);
            let s = sftp.clone();
            match self.block("mkdir", async move { s.create_dir(abs).await }) {
                Ok(()) => {}
                Err(e) => {
                    // v3 answers a generic Failure for "already exists" — confirm before propagating
                    match self.stat(&prefix)? {
                        Some(m) if m.kind == EntryKind::Dir => {}
                        _ => return Err(e),
                    }
                }
            }
        }
        Ok(())
    }

    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        use russh_sftp::protocol::OpenFlags;
        let conn = self.conn()?;
        let (parent, base) = crate::foundation::path::split_parent(rel);
        let tmp_rel = format!(
            "{parent}{}{base}.{}",
            crate::foundation::names::TEMP_PREFIX,
            std::process::id()
        );
        let tmp_abs = self.abs(&tmp_rel);
        let dst_abs = self.abs(rel);
        let sftp = conn.sftp.clone();
        // A previous interruption may have left debris under the same name
        {
            let s = sftp.clone();
            let t = tmp_abs.clone();
            let _ = self.block("clear stale temp", async move { s.remove_file(t).await });
        }
        let s2 = sftp.clone();
        let t2 = tmp_abs.clone();
        // CREATE|EXCL: the server's own O_EXCL refuses to overwrite (the FFS reliance)
        let file = self.block("open staged", async move {
            s2.open_with_flags(t2, OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::EXCLUDE).await
        })?;
        Ok(Box::new(SftpStaged {
            rt: self.rt.handle().clone(),
            timeout: self.timeout,
            sftp,
            tmp_abs,
            dst_abs,
            file: Some(file),
            hint: hint.clone(),
            committed: false,
        }))
    }

    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        let sftp = self.conn()?.sftp.clone();
        let (f, t) = (self.abs(from_rel), self.abs(to_rel));
        // v3 semantics: refuses an existing target (declared in caps; relied upon)
        self.block("rename", async move { sftp.rename(f, t).await })
    }

    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        self.block("remove_file", async move { sftp.remove_file(p).await })
    }

    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        match self.block("remove_dir", async move { sftp.remove_dir(p).await }) {
            Ok(()) => Ok(()),
            Err(e) if e.kind == VfsErrorKind::Protocol => {
                // v3's generic Failure: a non-empty directory is the overwhelmingly
                // common cause — classify by looking, never by guessing
                match self.read_dir_names(rel) {
                    Ok(l) if !l.is_empty() => Err(VfsError::new(
                        VfsErrorKind::NotEmpty,
                        format!("directory not empty: {rel}"),
                    )),
                    _ => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        let secs = (mtime_ms / 1000) as u32;
        // setstat by PATH, after any handle is closed — fsetstat corrupts on some
        // servers (Synology; the FFS finding)
        let attrs = FileAttributes { mtime: Some(secs), atime: Some(secs), ..attrs_none() };
        self.block("set_mtime", async move { sftp.set_metadata(p, attrs).await })
    }

    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        let attrs = FileAttributes { permissions: Some(mode), ..attrs_none() };
        self.block("chmod", async move { sftp.set_metadata(p, attrs).await })
    }

    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        let sftp = self.conn()?.sftp.clone();
        let link = self.abs(rel);
        let t = target.to_string();
        self.block("symlink", async move { sftp.symlink(t, link).await })
    }

    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        // Deliberately never asked: statvfs permanently corrupts sessions on some
        // servers (Mikrotik, per the FFS survey). Preflight reports the gate as inert.
        Ok(None)
    }
}

/// The whole connect sequence, async side. Returns (session, sftp, root_abs, server line).
async fn connect_and_open(
    host: &str,
    port: u16,
    user: &str,
    creds: &Credentials,
    timeout: Duration,
    root_spec: &str,
    display: &str,
) -> VfsResult<(russh::client::Handle<HostCheck>, SftpSession, String, String)> {
    let config = Arc::new(russh::client::Config::default());
    let handler = HostCheck { host: host.to_string(), port };

    let mut session = match tokio::time::timeout(
        timeout,
        russh::client::connect(config, (host, port), handler),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(map_connect_err(e, host, display)),
        Err(_) => {
            return Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("connecting to {host}:{port} timed out after {timeout:?}"),
            ))
        }
    };

    // Auth chain, most-specific first; every failed rung is named in the final error.
    let mut tried: Vec<String> = Vec::new();
    let mut authed = false;

    let mut key_candidates: Vec<(PathBuf, bool)> = Vec::new(); // (path, explicit)
    if let Some(k) = &creds.keyfile {
        key_candidates.push((k.clone(), true));
    } else {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let ssh = PathBuf::from(home).join(".ssh");
            for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
                let p = ssh.join(name);
                if p.is_file() {
                    key_candidates.push((p, false));
                }
            }
        }
    }
    for (path, explicit) in key_candidates {
        match load_secret_key(&path, creds.passphrase.as_deref()) {
            Ok(key) => {
                let hash = session
                    .best_supported_rsa_hash()
                    .await
                    .ok()
                    .flatten()
                    .flatten();
                let k = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
                match session.authenticate_publickey(user, k).await {
                    Ok(r) if r.success() => {
                        authed = true;
                        tried.push(format!("key {} ✓", path.display()));
                        break;
                    }
                    Ok(_) => tried.push(format!("key {} (server refused)", path.display())),
                    Err(e) => tried.push(format!("key {} ({e})", path.display())),
                }
            }
            Err(e) => {
                let head = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.lines().next().map(|l| l.to_string()))
                    .unwrap_or_default();
                let hint = if head.contains("PuTTY") {
                    " — this is a PuTTY .ppk; export an OpenSSH-format key with puttygen"
                } else if head.starts_with("ssh-") || head.starts_with("ecdsa-") {
                    " — this is a PUBLIC key; point at the private one"
                } else {
                    ""
                };
                let line = format!("key {} unreadable: {e}{hint}", path.display());
                if explicit {
                    return Err(VfsError::new(VfsErrorKind::Auth, line));
                }
                tried.push(line);
            }
        }
    }

    if !authed {
        if let Some(pw) = &creds.password {
            match session.authenticate_password(user, pw.clone()).await {
                Ok(r) if r.success() => {
                    authed = true;
                    tried.push("password ✓".into());
                }
                Ok(_) => tried.push("password (server refused)".into()),
                Err(e) => tried.push(format!("password ({e})")),
            }
        } else {
            tried.push("password (none stored — syncdash cred set can add one)".into());
        }
    }

    if !authed {
        return Err(VfsError::new(
            VfsErrorKind::Auth,
            format!("authentication to {user}@{host} failed; tried: {}", tried.join("; ")),
        ));
    }

    let channel = session
        .channel_open_session()
        .await
        .map_err(|e| VfsError::new(VfsErrorKind::Transient, format!("cannot open a channel: {e}")))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| VfsError::new(VfsErrorKind::Protocol, format!("the server refused the sftp subsystem: {e}")))?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| VfsError::new(VfsErrorKind::Protocol, format!("sftp handshake failed: {e}")))?;

    let root_abs = if root_spec.is_empty() {
        sftp.canonicalize(".")
            .await
            .map_err(|e| map_sftp_err("canonicalize home", e))?
    } else {
        format!("/{root_spec}")
    };

    let server_line = format!("sftp v3 via ssh, auth: {}", tried.last().cloned().unwrap_or_default());
    Ok((session, sftp, root_abs.trim_end_matches('/').to_string(), server_line))
}

fn map_connect_err(e: HostCheckError, host: &str, display: &str) -> VfsError {
    match e {
        HostCheckError::Changed { fingerprint } => VfsError::new(
            VfsErrorKind::Auth,
            format!(
                "!!! the host key for {host} DOES NOT MATCH ~/.ssh/known_hosts (now {fingerprint}) — possible man-in-the-middle; refusing. If the server was really reinstalled, remove the old line with ssh-keygen -R {host}"
            ),
        ),
        HostCheckError::Unknown { fingerprint } => VfsError::new(
            VfsErrorKind::Auth,
            format!(
                "{host} is not in ~/.ssh/known_hosts (its key is {fingerprint}) — connect once with `ssh {host}` to record it, then retry '{display}'"
            ),
        ),
        HostCheckError::Ssh(e) => {
            VfsError::new(VfsErrorKind::Transient, format!("ssh connection to {host} failed: {e}"))
        }
    }
}

/// Blocking `Read` over the async sftp file: the engine's hashing loops read through
/// this without knowing an event loop exists.
struct SftpRead {
    rt: tokio::runtime::Handle,
    timeout: Duration,
    file: russh_sftp::client::fs::File,
}

impl std::io::Read for SftpRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use tokio::io::AsyncReadExt;
        let d = self.timeout;
        let file = &mut self.file;
        match self.rt.clone().block_on(async { tokio::time::timeout(d, file.read(buf)).await }) {
            Ok(r) => r,
            Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "sftp read timed out")),
        }
    }
}

impl ReadStream for SftpRead {
    fn block_size(&self) -> usize {
        // FFS measured ~16×30000B as the knee of the SFTP throughput curve; russh-sftp
        // splits big reads into protocol-sized requests internally, this sizes our loop.
        480 * 1024
    }
}

/// The staged write on an sftp root: temp name in the destination's own directory
/// (server-side rename stays same-volume), opened CREATE|EXCL so the server's own
/// O_EXCL refuses collisions, landed by unlink-then-rename (v3 rename refuses an
/// existing target — the deliberate FFS reliance). mtime and mode go on by PATH
/// after the rename; their failures ride the CommitReport, never fail the copy.
struct SftpStaged {
    rt: tokio::runtime::Handle,
    timeout: Duration,
    sftp: Arc<SftpSession>,
    tmp_abs: String,
    dst_abs: String,
    file: Option<russh_sftp::client::fs::File>,
    hint: WriteHint,
    committed: bool,
}

impl SftpStaged {
    fn block<F, T>(&self, what: &str, fut: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = Result<T, russh_sftp::client::error::Error>>,
    {
        let d = self.timeout;
        match self.rt.block_on(async { tokio::time::timeout(d, fut).await }) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(map_sftp_err(what, e)),
            Err(_) => Err(VfsError::new(VfsErrorKind::Transient, format!("{what} timed out after {d:?}"))),
        }
    }
}

impl WriteStaged for SftpStaged {
    fn write(&mut self, buf: &[u8]) -> VfsResult<()> {
        use tokio::io::AsyncWriteExt;
        let d = self.timeout;
        let f = self.file.as_mut().expect("write after seal");
        match self.rt.block_on(async { tokio::time::timeout(d, f.write_all(buf)).await }) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(VfsError::new(VfsErrorKind::Transient, format!("sftp write failed: {e}"))),
            Err(_) => Err(VfsError::new(VfsErrorKind::Transient, "sftp write timed out")),
        }
    }

    fn block_size(&self) -> usize {
        480 * 1024
    }

    fn write_at(&mut self, _off: u64, _buf: &[u8]) -> VfsResult<()> {
        Err(VfsError::unsupported("random-access writes are not offered on sftp roots (delta is a both-local affair)"))
    }

    fn seal(&mut self, fsync: bool) -> VfsResult<()> {
        use tokio::io::AsyncWriteExt;
        let d = self.timeout;
        if let Some(mut f) = self.file.take() {
            let res = self.rt.block_on(async {
                tokio::time::timeout(d, async {
                    f.flush().await?;
                    if fsync {
                        // fsync@openssh.com — where the server lacks the extension this
                        // fails, and per the preflight NeedsAck line that fails the file
                        f.sync_all()
                            .await
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                    }
                    f.shutdown().await
                })
                .await
            });
            match res {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(VfsError::new(
                    if fsync { VfsErrorKind::Protocol } else { VfsErrorKind::Transient },
                    format!("sealing the staged file failed: {e}"),
                )),
                Err(_) => Err(VfsError::new(VfsErrorKind::Transient, "seal timed out")),
            }
        } else {
            Ok(())
        }
    }

    fn staged_len(&self) -> VfsResult<u64> {
        let s = self.sftp.clone();
        let p = self.tmp_abs.clone();
        let a = self.block("staged_len", async move { s.symlink_metadata(p).await })?;
        Ok(a.size.unwrap_or(0))
    }

    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>> {
        let s = self.sftp.clone();
        let p = self.tmp_abs.clone();
        let file = self.block("open staged for read-back", async move { s.open(p).await })?;
        Ok(Box::new(SftpRead { rt: self.rt.clone(), timeout: self.timeout, file }))
    }

    fn commit(mut self: Box<Self>) -> VfsResult<CommitReport> {
        if self.file.is_some() {
            self.seal(false)?;
        }
        // Clear the destination, then rename — v3 rename refuses an existing target,
        // which is exactly the atomicity contract we lean on
        {
            let s = self.sftp.clone();
            let dst = self.dst_abs.clone();
            match self.block("clear destination", async move { s.remove_file(dst).await }) {
                Ok(()) => {}
                Err(e) if e.kind == VfsErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        {
            let s = self.sftp.clone();
            let (t, dst) = (self.tmp_abs.clone(), self.dst_abs.clone());
            self.block("rename into place", async move { s.rename(t, dst).await })?;
        }
        self.committed = true;

        let mut report = CommitReport::default();
        if self.hint.mtime_ms.is_some() || self.hint.mode.is_some() {
            let secs = self.hint.mtime_ms.map(|ms| (ms / 1000) as u32);
            let attrs = FileAttributes {
                mtime: secs,
                atime: secs,
                permissions: self.hint.mode,
                ..attrs_none()
            };
            let s = self.sftp.clone();
            let dst = self.dst_abs.clone();
            if let Err(e) = self.block("setstat after rename", async move { s.set_metadata(dst, attrs).await }) {
                report.mtime_error = Some(e);
            } else if self.hint.mtime_ms.is_some() {
                let s2 = self.sftp.clone();
                let dst2 = self.dst_abs.clone();
                if let Ok(a) = self.block("stat back", async move { s2.symlink_metadata(dst2).await }) {
                    report.mtime_ondisk_ms = a.mtime.map(|s| s as i64 * 1000);
                }
            }
        }
        Ok(report)
    }
}

impl Drop for SftpStaged {
    fn drop(&mut self) {
        if !self.committed {
            use tokio::io::AsyncWriteExt;
            let d = self.timeout;
            if let Some(mut f) = self.file.take() {
                let _ = self.rt.block_on(async { tokio::time::timeout(d, f.shutdown()).await });
            }
            let s = self.sftp.clone();
            let t = self.tmp_abs.clone();
            let _ = self
                .rt
                .block_on(async { tokio::time::timeout(d, async move { s.remove_file(t).await }).await });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::spec::{parse, RootSpec};

    fn backend(s: &str) -> SftpBackend {
        let RootSpec::Remote(r) = parse(s) else { panic!() };
        SftpBackend::new(r, crate::fs::vfs::cred::default_provider())
    }

    #[test]
    fn caps_declare_the_sftp_profile() {
        let b = backend("sftp://ben@host/data");
        let c = b.caps();
        assert_eq!(c.mtime_precision_ms, 1000);
        assert_eq!(c.rename_overwrite, Support::No);
        assert_eq!(c.free_space, Support::No);
        assert!(c.ranged_read.yes());
    }

    #[test]
    fn unconnected_backend_reports_transient_not_missing() {
        let b = backend("sftp://ben@host/data");
        let e = b.stat("x").unwrap_err();
        assert_eq!(e.kind, VfsErrorKind::Transient);
    }

    #[test]
    fn write_side_still_needs_a_connection_first() {
        let b = backend("sftp://ben@host/data");
        let e = b.remove_file("x").unwrap_err();
        assert_eq!(e.kind, VfsErrorKind::Transient, "unconnected is a transient state, never a judgment about files");
    }
}
