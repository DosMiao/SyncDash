//! The SFTP backend — pure-Rust russh + russh-sftp behind the sync `Vfs` trait.
//!
//! Why russh and not libssh2 bindings: this machine's users hold OpenSSH-format keys
//! (the only format ssh-keygen has emitted since 7.8), which libssh2's WinCNG backend
//! cannot read, and building its OpenSSL alternative needs a perl toolchain Windows
//! boxes don't have. russh reads those keys natively and pulls zero C dependencies
//! (ring backend). The FFS survey's *protocol*-level lessons still apply — SFTP v3
//! rename refuses existing targets, setstat-by-path after close, no statvfs — they
//! were never libssh2-specific.
//! Root-lock claims require the stronger cross-session guarantee supplied by
//! `hardlink@openssh.com`; a connected backend that cannot establish that capability remains
//! readable but is blocked from apply before the first mutation.
//!
//! Concurrency model: one SSH session, one SFTP channel, requests multiplexed by the
//! protocol's own ids (russh-sftp is fully concurrent). The tokio runtime is private
//! to this backend — two worker threads, entered only through `block_on` from engine
//! threads; the trait stays sync and the engine never sees an async type.
//!
//! The handshake is not here: connect, host-key check and the auth chain live in `fs::ssh`,
//! shared with the peer lane, which reaches the same hosts with the same keys.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::spec::EndpointSpec;
use super::VfsEntryKind;
use super::{
    CaseSense, Credentials, ReadStream, Support, VDirEntry, VMeta, Vfs, VfsCaps, WriteHint,
    WriteStaged,
};
use crate::foundation::names::TEMP_PREFIX;

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, StatusCode};

use crate::fs::ssh::HostCheck;

mod staged;

use staged::{SftpRead, SftpStaged};

pub struct SftpBackend {
    spec: EndpointSpec,
    rt: tokio::runtime::Runtime,
    timeout: Duration,
    conn: OnceLock<Conn>,
    connect_gate: Mutex<()>,
    server_line: Mutex<Option<String>>,
    exclusive_staged_file_publish: Mutex<Support>,
}

struct Conn {
    /// Kept alive for the session's sake; the sftp channel is what we talk to.
    _session: russh::client::Handle<HostCheck>,
    sftp: Arc<SftpSession>,
    /// Absolute server-side root ('/'-separated, no trailing '/').
    root_abs: String,
}

impl SftpBackend {
    pub fn new(spec: EndpointSpec) -> SftpBackend {
        let timeout = spec.timeout();
        let rt = super::backend_runtime("syncdash-sftp");
        SftpBackend {
            spec,
            rt,
            timeout,
            conn: OnceLock::new(),
            connect_gate: Mutex::new(()),
            server_line: Mutex::new(None),
            exclusive_staged_file_publish: Mutex::new(Support::Unknown),
        }
    }

    fn conn(&self) -> VfsResult<&Conn> {
        self.conn.get().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Transient,
                format!(
                    "'{}' is not connected — connect() must run first",
                    self.spec.display()
                ),
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

    /// Run one sftp operation with the backend's timeout, translating the protocol's own error
    /// into a `VfsError`.
    fn block<F, T>(&self, what: &str, fut: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = Result<T, russh_sftp::client::error::Error>>,
    {
        super::block_with_timeout(
            self.rt.handle(),
            self.timeout,
            what,
            Some(&self.spec.display()),
            async move { fut.await.map_err(|e| map_sftp_err(what, e)) },
        )
    }

    /// Absence is confirmed by [`super::absence::confirm_absent`]; this backend supplies only
    /// its own parent listing. The blocking call and its timeout stay inside the closure, because
    /// a listing that times out must reach the shared rule as an error, not as an empty directory.
    fn confirm_absent(&self, rel: &str) -> VfsResult<()> {
        super::absence::confirm_absent(rel, |parent| {
            let sftp = self.conn()?.sftp.clone();
            let p = self.abs(parent);
            self.block(
                "read_dir (absence check)",
                async move { sftp.read_dir(p).await },
            )
            .map(|dir| {
                dir.into_iter()
                    .map(|entry| entry.file_name().to_owned())
                    .collect()
            })
        })
    }
}

/// An attributes packet that asserts NOTHING. Never use `FileAttributes::default()`
/// for setstat: that crate's Default is `size: Some(0), uid/gid: Some(0),
/// permissions: Some(0o777|DIR), mtime: Some(0)` — sent as-is it TRUNCATES the file
/// to zero and clobbers its owner and mode. Found the hard way: a live apply landed
/// every file at 0 bytes after a "successful" rename.
pub(super) fn attrs_none() -> FileAttributes {
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
        VfsEntryKind::Directory
    } else if a.is_symlink() {
        VfsEntryKind::Symlink
    } else {
        VfsEntryKind::File
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

pub(super) fn map_sftp_err(what: &str, e: russh_sftp::client::error::Error) -> VfsError {
    use russh_sftp::client::error::Error as E;
    match e {
        E::Status(s) => {
            let kind = match s.status_code {
                StatusCode::NoSuchFile => VfsErrorKind::NotFound, // callers double-check before trusting this
                StatusCode::PermissionDenied => VfsErrorKind::PermissionDenied,
                _ => VfsErrorKind::Protocol,
            };
            VfsError::new(
                kind,
                format!(
                    "{what}: server answered {}: {}",
                    s.status_code, s.error_message
                ),
            )
        }
        E::Timeout => VfsError::new(
            VfsErrorKind::Transient,
            format!("{what}: sftp request timed out"),
        ),
        other => VfsError::new(VfsErrorKind::Transient, format!("{what}: {other}")),
    }
}

impl Vfs for SftpBackend {
    fn caps(&self) -> VfsCaps {
        VfsCaps {
            protocol: "sftp",
            mtime_precision_ms: 1000, // v3 attributes carry whole seconds
            set_mtime: Support::Yes,  // setstat-by-path (write lane)
            // fsync@openssh.com is never probed: `seal(true)` asks for it and the server either
            // honours it or fails that file, which is the only moment the answer matters.
            fsync: Support::Unknown,
            rename_overwrite: Support::No, // v3 rename refuses an existing target (relied upon)
            exclusive_staged_file_publish: *self.exclusive_staged_file_publish.lock().unwrap(),
            exclusive_entry_rename: Support::Unknown,
            exclusive_symlink_publish: Support::Unknown,
            durable_namespace: Support::Unknown,
            ranged_read: Support::Yes, // seekable file handles → real sampled evidence
            write_at: Support::No,     // staged writes are sequential; delta is a both-local affair
            unix_mode: Support::Yes,
            symlink: Support::Yes,
            file_id: Support::No,
            free_space: Support::No, // statvfs corrupts sessions on some servers (FFS finding); never asked
            read_back: Support::Yes,
            medium: super::Medium::NetworkShare,
            local_trash: false,
            case_sensitivity: CaseSense::Unknown,
            // The server may well be Windows OpenSSH; the banner does not prove it either way.
            name_rules: super::NameRules::Unknown,
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
                format!(
                    "'{}' names no user — spell it sftp://user@host/…",
                    self.spec.display()
                ),
            )
        })?;
        let creds = super::cred::credentials_for(&self.spec)?;
        let timeout = self.timeout;
        let root_spec = self.spec.root.clone();
        let display = self.spec.display();

        let built = self.rt.block_on(async {
            connect_and_open(&host, port, &user, &creds, timeout, &root_spec, &display).await
        })?;
        *self.server_line.lock().unwrap() = Some(built.3);
        *self.exclusive_staged_file_publish.lock().unwrap() = built.4;
        let _ = self.conn.set(Conn {
            _session: built.0,
            sftp: Arc::new(built.1),
            root_abs: built.2,
        });
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
            let name = crate::foundation::path::EntryName::try_from(name).map_err(|e| {
                VfsError::new(
                    VfsErrorKind::Protocol,
                    format!("SFTP server returned an invalid directory entry: {e}"),
                )
            })?;
            out.push(VDirEntry {
                name,
                meta: meta_of(&e.metadata()),
            });
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
        let res = self.rt.block_on(async move {
            tokio::time::timeout(timeout, async move {
                let mut f = sftp.open(p).await?;
                f.seek(std::io::SeekFrom::Start(off))
                    .await
                    .map_err(russh_sftp::client::error::Error::from)?;
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
            })
            .await
        });
        match res {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(map_sftp_err("read_range", e)),
            Err(_) => Err(VfsError::new(
                VfsErrorKind::Transient,
                "read_range timed out",
            )),
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
                        Some(m) if m.kind == VfsEntryKind::Directory => {}
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
        let tmp_rel = super::staged_temp_rel(rel)?;
        let tmp_abs = self.abs(&tmp_rel);
        let dst_abs = self.abs(rel);
        let sftp = conn.sftp.clone();
        let s2 = sftp.clone();
        let t2 = tmp_abs.clone();
        // CREATE|EXCL: the server's own O_EXCL refuses to overwrite (the FFS reliance)
        let file = self.block("open staged", async move {
            s2.open_with_flags(
                t2,
                OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::EXCLUDE,
            )
            .await
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
        // v3 semantics: refuses an existing target (declared in caps as rename_overwrite: No, and
        // relied upon). It signals that refusal with the same generic Failure it uses for
        // everything else, so — exactly as `remove_dir` below does with a non-empty directory —
        // classify by looking, never by guessing. Callers are entitled to tell "the destination is
        // occupied" apart from "the link broke": the first is an ordinary outcome they recover
        // from by staging elsewhere, the second is not.
        match self.block("rename", async move { sftp.rename(f, t).await }) {
            Ok(()) => Ok(()),
            Err(e) if e.kind == VfsErrorKind::Protocol => match self.stat(to_rel) {
                Ok(Some(_)) => Err(VfsError::new(
                    VfsErrorKind::AlreadyExists,
                    format!("rename target already exists: {to_rel}"),
                )),
                _ => Err(e),
            },
            Err(e) => Err(e),
        }
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
        let attrs = FileAttributes {
            mtime: Some(secs),
            atime: Some(secs),
            ..attrs_none()
        };
        self.block(
            "set_mtime",
            async move { sftp.set_metadata(p, attrs).await },
        )
    }

    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        let sftp = self.conn()?.sftp.clone();
        let p = self.abs(rel);
        let attrs = FileAttributes {
            permissions: Some(mode),
            ..attrs_none()
        };
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

/// The whole connect sequence, async side. Returns the session, SFTP client, root, server line,
/// and negotiated exclusive staged-file publication support.
///
/// The handshake itself — connect, host-key check, auth chain — is `fs::ssh`, shared with the peer
/// lane. What is left here is the part that is actually sftp: requesting the subsystem and
/// resolving the login home when the phrase named no path.
async fn connect_and_open(
    host: &str,
    port: u16,
    user: &str,
    creds: &Credentials,
    timeout: Duration,
    root_spec: &str,
    display: &str,
) -> VfsResult<(
    russh::client::Handle<HostCheck>,
    SftpSession,
    String,
    String,
    Support,
)> {
    let session = crate::fs::ssh::connect(host, port, user, creds, timeout, display).await?;
    let auth = session.auth_summary();
    let session = session.handle;

    let channel = session.channel_open_session().await.map_err(|e| {
        VfsError::new(
            VfsErrorKind::Transient,
            format!("cannot open a channel: {e}"),
        )
    })?;
    channel.request_subsystem(true, "sftp").await.map_err(|e| {
        VfsError::new(
            VfsErrorKind::Protocol,
            format!("the server refused the sftp subsystem: {e}"),
        )
    })?;
    let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| {
        VfsError::new(
            VfsErrorKind::Protocol,
            format!("sftp handshake failed: {e}"),
        )
    })?;

    let root_abs = if root_spec.is_empty() {
        sftp.canonicalize(".")
            .await
            .map_err(|e| map_sftp_err("canonicalize home", e))?
    } else {
        format!("/{root_spec}")
    };

    let root_abs = root_abs.trim_end_matches('/').to_string();
    let exclusive_staged_file_publish = probe_hardlink_support(&sftp, &root_abs).await?;
    let server_line = format!(
        "sftp v3 via ssh, auth: {auth}, hardlink@openssh.com: {}",
        exclusive_staged_file_publish.as_str()
    );
    Ok((
        session,
        sftp,
        root_abs,
        server_line,
        exclusive_staged_file_publish,
    ))
}

async fn probe_hardlink_support(sftp: &SftpSession, root_abs: &str) -> VfsResult<Support> {
    let token = super::random_name_token()?;
    let missing_parent = if root_abs.is_empty() {
        format!("/{TEMP_PREFIX}hardlink-probe.{token}")
    } else {
        format!("{root_abs}/{TEMP_PREFIX}hardlink-probe.{token}")
    };
    let source = format!("{missing_parent}/source");
    let destination = format!("{missing_parent}/destination");
    match sftp.hardlink(source, destination.clone()).await {
        Ok(false) => Ok(Support::No),
        Ok(true) => {
            let _ = sftp.remove_file(destination).await;
            Ok(Support::Yes)
        }
        Err(russh_sftp::client::error::Error::Status(status))
            if status.status_code == StatusCode::NoSuchFile =>
        {
            Ok(Support::Yes)
        }
        Err(russh_sftp::client::error::Error::Status(status))
            if status.status_code == StatusCode::OpUnsupported =>
        {
            Ok(Support::No)
        }
        Err(_) => Ok(Support::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::spec::{parse, RootSpec};

    fn backend(s: &str) -> SftpBackend {
        let RootSpec::Endpoint(r) = parse(s) else {
            panic!()
        };
        SftpBackend::new(r)
    }

    #[test]
    fn caps_declare_the_sftp_profile() {
        let b = backend("sftp://ben@host/data");
        let c = b.caps();
        assert_eq!(c.mtime_precision_ms, 1000);
        assert_eq!(c.rename_overwrite, Support::No);
        assert_eq!(c.exclusive_staged_file_publish, Support::Unknown);
        assert_eq!(c.exclusive_entry_rename, Support::Unknown);
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
        assert_eq!(
            e.kind,
            VfsErrorKind::Transient,
            "unconnected is a transient state, never a judgment about files"
        );
    }
}
