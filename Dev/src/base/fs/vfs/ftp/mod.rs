//! The FTP backend — suppaftp's sync stream behind the `Vfs` trait.
//!
//! FTP is the zoo of the protocol family (the FFS survey's word), and this backend
//! spends most of its lines on honesty about that:
//! - listing happens as CWD + a bare MLSD/LIST — a pathname argument to MLSD gets
//!   glob-expanded by some servers (ProFTPD), so it never gets one; every *other*
//!   operation uses absolute paths and never depends on CWD state;
//! - `stat` IS a parent listing (FTP has no stat) — which makes its absence
//!   self-confirming, the Transient/NotFound doctrine built in;
//! - timestamps: MLSD carries UTC seconds (precision 1000 ms); a LIST-only server
//!   thinks in minutes and pretends UTC (60000 ms, FileZilla's stance) — declared in
//!   caps, so the compare window widens out loud;
//! - MFMT is FEAT-gated. Without it mtimes cannot be set — the correction table
//!   absorbs that for compare. FTP remains read-only for apply independently of MFMT because the
//!   protocol has no exclusive staged-file publication primitive for a root-lock claim;
//! - one control connection, operations serialized (`max_parallel_streams = 1`) — and the apply
//!   lane clamps to that, because a second concurrent transfer here does not queue, it fails;
//! - **no ranged reads, whatever FEAT says about REST.** Nothing in FTP ends a partial transfer
//!   cleanly, and the unread completion reply desynchronizes the one control channel — so sampled
//!   evidence is declined and both sides of a comparison read whole files instead.
//!
//! `ftps://` upgrades the control connection with AUTH TLS before the login goes out (explicit
//! FTPS, not the deprecated implicit kind on port 990). Certificates are checked against the
//! **operating system's** trust store — see `tls_connector` for why that rather than a bundled
//! root set, and for why there is no flag to turn verification off.

use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::spec::EndpointSpec;
use super::VfsEntryKind;
use super::{
    CaseSense, CredentialProvider, ReadStream, Support, VDirEntry, VMeta, Vfs, VfsCaps, WriteHint,
    WriteStaged,
};

use suppaftp::list::File as FtpFile;
use suppaftp::types::FileType;
use suppaftp::{FtpError, FtpStream, Mode};

mod staged;

use staged::{FtpRead, FtpStaged};

#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Feats {
    pub(super) mlsd: bool,
    pub(super) mfmt: bool,
    pub(super) rest: bool,
}

/// The control connection, plain or TLS.
///
/// suppaftp parameterizes the stream on its TLS type — `FtpStream` is `ImplFtpStream<NoTlsStream>`
/// and `into_secure` hands back an `ImplFtpStream<RustlsStream>` — so the two are different types
/// all the way down and cannot live in one field. Both carry identical method signatures, though,
/// so the split is absorbed here in one forwarder per method the backend uses, and **every call
/// site stays exactly as it was**. The alternative was making the whole backend generic and
/// spreading `<T>` through sixteen call sites and two structs to say one thing.
///
/// The data streams are the reason this stays small: `retr_as_stream` yields `DataStream<T>`,
/// which is also parameterized, but `finalize_*` and `abort` take `impl Read` / `impl Write`
/// rather than the concrete type — so the boxes `staged.rs` already uses cross the split
/// unchanged.
pub(super) enum Stream {
    Plain(FtpStream),
    Tls(Box<suppaftp::RustlsFtpStream>),
}

/// One forwarder per method, both arms identical. Written out rather than macro'd: eleven lines
/// a reader can check against the call sites beats a macro they have to expand in their head.
macro_rules! fwd {
    ($self:ident, $m:ident $(, $a:expr)*) => {
        match $self {
            Stream::Plain(s) => s.$m($($a),*),
            Stream::Tls(s) => s.$m($($a),*),
        }
    };
}

impl Stream {
    pub(super) fn cwd(&mut self, p: &str) -> Result<(), FtpError> {
        fwd!(self, cwd, p)
    }
    pub(super) fn mkdir(&mut self, p: &str) -> Result<(), FtpError> {
        fwd!(self, mkdir, p)
    }
    pub(super) fn rm(&mut self, p: &str) -> Result<(), FtpError> {
        fwd!(self, rm, p)
    }
    pub(super) fn rmdir(&mut self, p: &str) -> Result<(), FtpError> {
        fwd!(self, rmdir, p)
    }
    pub(super) fn rename(&mut self, from: &str, to: &str) -> Result<(), FtpError> {
        fwd!(self, rename, from, to)
    }
    pub(super) fn list(&mut self, p: Option<&str>) -> Result<Vec<String>, FtpError> {
        fwd!(self, list, p)
    }
    pub(super) fn mlsd(&mut self, p: Option<&str>) -> Result<Vec<String>, FtpError> {
        fwd!(self, mlsd, p)
    }
    /// Boxed because the concrete `DataStream<T>` differs per arm — the same erasure
    /// `staged.rs` already relies on.
    pub(super) fn retr_as_stream(&mut self, p: &str) -> Result<Box<dyn Read + Send>, FtpError> {
        Ok(match self {
            Stream::Plain(s) => Box::new(s.retr_as_stream(p)?) as Box<dyn Read + Send>,
            Stream::Tls(s) => Box::new(s.retr_as_stream(p)?) as Box<dyn Read + Send>,
        })
    }
    pub(super) fn put_with_stream(
        &mut self,
        p: &str,
    ) -> Result<Box<dyn std::io::Write + Send>, FtpError> {
        Ok(match self {
            Stream::Plain(s) => Box::new(s.put_with_stream(p)?) as Box<dyn std::io::Write + Send>,
            Stream::Tls(s) => Box::new(s.put_with_stream(p)?) as Box<dyn std::io::Write + Send>,
        })
    }
    pub(super) fn finalize_retr_stream(&mut self, r: impl Read) -> Result<(), FtpError> {
        fwd!(self, finalize_retr_stream, r)
    }
    pub(super) fn finalize_put_stream(&mut self, w: impl std::io::Write) -> Result<(), FtpError> {
        fwd!(self, finalize_put_stream, w)
    }
    pub(super) fn abort(&mut self, r: impl Read + 'static) -> Result<(), FtpError> {
        fwd!(self, abort, r)
    }
    pub(super) fn login(&mut self, user: &str, pass: &str) -> Result<(), FtpError> {
        fwd!(self, login, user, pass)
    }
    pub(super) fn feat(&mut self) -> Result<suppaftp::types::Features, FtpError> {
        fwd!(self, feat)
    }
    pub(super) fn opts(&mut self, o: &str, v: Option<&str>) -> Result<(), FtpError> {
        fwd!(self, opts, o, v)
    }
    pub(super) fn transfer_type(&mut self, t: FileType) -> Result<(), FtpError> {
        fwd!(self, transfer_type, t)
    }
    pub(super) fn set_mode(&mut self, m: Mode) {
        fwd!(self, set_mode, m)
    }
    pub(super) fn size(&mut self, p: &str) -> Result<usize, FtpError> {
        fwd!(self, size, p)
    }
    pub(super) fn custom_command(
        &mut self,
        cmd: impl ToString,
        ok: &[suppaftp::Status],
    ) -> Result<suppaftp::types::Response, FtpError> {
        fwd!(self, custom_command, cmd, ok)
    }
}

/// The TLS client config for `ftps://`, built on the **operating system's** trust store.
///
/// The choice between this and a bundled root set (`webpki-roots`) is the one real decision here,
/// and it follows the precedent `fs::ssh` already set with `known_hosts`: reuse the trust the user
/// has established rather than shipping a second, parallel one. Concretely, the FTPS server on a
/// LAN NAS usually presents a certificate its owner installed themselves — a bundled Mozilla root
/// set rejects exactly that, which is the main case this exists for, while the OS store accepts it
/// *because the user already said so somewhere they can see and revoke*.
///
/// No `insecure_tls` escape hatch: a flag that turns verification off is a flag that ends up set.
/// A certificate this refuses is a certificate to install, and the error says which.
/// An AUTH TLS failure, told as something the operator can act on.
///
/// Two things the generic mapper gets wrong here. A refused certificate is not `Transient` — it
/// will be refused identically on every retry until somebody installs something, so it belongs
/// with a refused password under `Auth`. And `tls_connector` above promises that "a certificate
/// this refuses is a certificate to install, and the error says which"; rustls reports the
/// *reason* but hands back no chain to print, so the message carries the command that shows the
/// operator the exact certificate being refused. Naming the remedy is what makes having no
/// `insecure_tls` flag a reasonable position rather than a dead end.
fn map_tls_err(host: &str, port: u16, e: FtpError) -> VfsError {
    let text = e.to_string();
    if !text.contains("invalid peer certificate") && !text.contains("CertificateError") {
        return map_ftp_err("AUTH TLS", e);
    }
    let why = if text.contains("UnknownIssuer") {
        "signed by an issuer this machine does not trust"
    } else if text.contains("Expired") {
        "that has expired"
    } else if text.contains("NotValidForName") {
        "that is not valid for this host"
    } else {
        "this machine's trust store will not accept"
    };
    VfsError::new(
        VfsErrorKind::Auth,
        format!(
            "ftps://{host}:{port} presented a certificate {why} ({text}). There is deliberately no \
             flag to skip verification — install the issuing certificate into this machine's trust \
             store instead, where you can see it and revoke it. To read the certificate being \
             refused: openssl s_client -starttls ftp -connect {host}:{port} -showcerts"
        ),
    )
}

fn tls_connector() -> VfsResult<suppaftp::RustlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for cert in loaded.certs {
        // A store with one unparseable certificate in it is still a usable store; refusing the
        // whole connection over one bad entry would be worse than skipping it.
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err(VfsError::new(
            VfsErrorKind::Io,
            format!(
                "no trusted root certificates could be read from this machine's store{} — ftps:// cannot verify anything without them",
                loaded
                    .errors
                    .first()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            ),
        ));
    }
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(suppaftp::RustlsConnector::from(std::sync::Arc::new(cfg)))
}

pub(super) struct FtpConn {
    stream: Stream,
}

pub(super) type ConnSlot = Arc<Mutex<Option<FtpConn>>>;
pub struct FtpBackend {
    spec: EndpointSpec,
    creds: Arc<dyn CredentialProvider>,
    timeout: Duration,
    conn: ConnSlot,
    feats: OnceLock<Feats>,
}

pub(super) fn map_ftp_err(what: &str, e: FtpError) -> VfsError {
    match e {
        FtpError::ConnectionError(io) => VfsError::new(
            VfsErrorKind::Transient,
            format!("{what}: connection error: {io}"),
        ),
        FtpError::UnexpectedResponse(resp) => {
            let code = resp.status as u32;
            let body = String::from_utf8_lossy(&resp.body).trim().to_string();
            let kind = match code {
                550 => VfsErrorKind::NotFound, // "not found OR no permission" — callers confirm before believing
                _ => VfsErrorKind::Protocol,
            };
            VfsError::new(kind, format!("{what}: server answered {code}: {body}"))
        }
        other => VfsError::new(VfsErrorKind::Protocol, format!("{what}: {other}")),
    }
}

impl FtpBackend {
    pub fn new(spec: EndpointSpec, creds: Arc<dyn CredentialProvider>) -> FtpBackend {
        let timeout = spec
            .opt("timeout")
            .and_then(|t| t.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(20));
        FtpBackend {
            spec,
            creds,
            timeout,
            conn: Arc::new(Mutex::new(None)),
            feats: OnceLock::new(),
        }
    }

    fn abs(&self, rel: &str) -> String {
        let root = self.spec.root.trim_matches('/');
        match (root.is_empty(), rel.is_empty()) {
            (true, true) => "/".into(),
            (true, false) => format!("/{rel}"),
            (false, true) => format!("/{root}"),
            (false, false) => format!("/{root}/{rel}"),
        }
    }

    fn with_conn<T>(
        &self,
        what: &str,
        f: impl FnOnce(&mut FtpConn) -> Result<T, FtpError>,
    ) -> VfsResult<T> {
        let mut guard = self.conn.lock().unwrap();
        let conn = guard.as_mut().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Transient,
                format!(
                    "'{}' is not connected — connect() must run first",
                    self.spec.display()
                ),
            )
        })?;
        f(conn).map_err(|e| {
            // A connection-layer failure poisons the session: drop it so the next
            // op reconnects instead of talking into a dead socket
            let v = map_ftp_err(what, e);
            if v.kind == VfsErrorKind::Transient {
                *guard = None;
            }
            v
        })
    }

    /// List one directory (CWD + bare MLSD/LIST — never a pathname argument).
    fn list_dir(&self, rel: &str) -> VfsResult<Vec<FtpFile>> {
        let abs = self.abs(rel);
        let mlsd = self.feats.get().map(|f| f.mlsd).unwrap_or(false);
        let lines = self.with_conn("read_dir", |c| {
            c.stream.cwd(&abs)?;
            if mlsd {
                c.stream.mlsd(None)
            } else {
                c.stream.list(None)
            }
        })?;
        let mut out = Vec::new();
        for line in &lines {
            // (The crate's deprecation note names a `FileParser` that does not exist;
            // the real struct is ListParser)
            let parsed = if mlsd {
                suppaftp::list::ListParser::parse_mlsd(line)
            } else {
                FtpFile::try_from(line.as_str())
            };
            match parsed {
                Ok(f) => {
                    if f.name() != "." && f.name() != ".." {
                        out.push(f);
                    }
                }
                Err(_) => {
                    // A line the parser cannot read must not vanish silently — it would
                    // make that entry look absent. Refuse the listing instead.
                    return Err(VfsError::new(
                        VfsErrorKind::Protocol,
                        format!("unparseable listing line from the server in '{rel}': {line}"),
                    ));
                }
            }
        }
        Ok(out)
    }
}

fn meta_of(f: &FtpFile) -> VMeta {
    let kind = if f.is_directory() {
        VfsEntryKind::Directory
    } else if f.is_symlink() {
        VfsEntryKind::Symlink
    } else {
        VfsEntryKind::File
    };
    VMeta {
        kind,
        size: f.size() as u64,
        mtime_ms: crate::foundation::time::systime_ms(f.modified()),
        mode: None,
        file_id: None,
        link: f.symlink().map(|p| p.to_string_lossy().into_owned()),
    }
}

impl Vfs for FtpBackend {
    fn caps(&self) -> VfsCaps {
        let f = self.feats.get().copied();
        VfsCaps {
            protocol: "ftp",
            mtime_precision_ms: match f {
                Some(f) if f.mlsd => 1000,
                Some(_) => 60_000, // LIST-only: minutes, pretending UTC
                None => 60_000,    // unknown until connect — assume the worst, honestly
            },
            set_mtime: match f {
                Some(f) if f.mfmt => Support::Yes,
                Some(_) => Support::No,
                None => Support::Unknown,
            },
            fsync: Support::No,
            rename: Support::Yes,
            rename_overwrite: Support::Unknown, // varies by server; the engine clears targets anyway
            exclusive_staged_file_publish: Support::No,
            exclusive_entry_rename: Support::Unknown,
            exclusive_symlink_publish: Support::No,
            durable_namespace: Support::No,
            // REST positions a transfer, but nothing in FTP *ends* one early and cleanly: the
            // client has to stop a RETR it no longer wants, and servers disagree about the reply.
            // Verified against pyftpdlib: both ABOR and closing the data socket leave a response
            // the client does not consume, and the next command receives it instead — a `stat`
            // answered "350 Restarting at position 0" from two calls earlier. On a single control
            // connection there is nowhere for that confusion to be contained, and every answer
            // afterwards decides whether a file gets deleted.
            //
            // So this says No rather than "Yes, usually". The joint-tier rule then reads both sides
            // in full, which is slower and correct, and the caps report says so out loud. Declaring
            // a capability the backend cannot honour is the worse failure: it degrades evidence
            // quietly, on exactly the large files sampling exists for.
            ranged_read: Support::No,
            write_at: Support::No,
            unix_mode: Support::No,
            symlink: Support::No, // reading them in listings works; creating them does not
            file_id: Support::No,
            free_space: Support::No,
            read_back: Support::Yes, // a full re-download — honest, and verify's cost on this backend
            medium: super::Medium::NetworkShare,
            local_trash: false,
            case_sensitivity: CaseSense::Unknown,
            // SYST hints at the server OS but lies often enough not to be evidence.
            name_rules: super::NameRules::Unknown,
            max_parallel_streams: 1, // one control connection, operations serialize
        }
    }

    fn display(&self) -> String {
        self.spec.display()
    }

    fn identity(&self) -> String {
        self.spec.identity()
    }

    fn server_info(&self) -> Option<String> {
        self.feats.get().map(|f| {
            // REST is reported because it is what the server said, not because this backend uses
            // it: ranged reads are declined regardless (see `read_range`), so it is a fact about
            // the server, never a promise about the evidence tier.
            format!(
                "ftp, MLSD:{} MFMT:{} REST:{} (unused)",
                if f.mlsd {
                    "yes"
                } else {
                    "NO (minute-precision LIST)"
                },
                if f.mfmt {
                    "yes"
                } else {
                    "NO (mtimes cannot be set)"
                },
                if f.rest { "yes" } else { "no" },
            )
        })
    }

    fn connect(&self) -> VfsResult<()> {
        let mut guard = self.conn.lock().unwrap();
        if guard.is_some() {
            return Ok(());
        }
        let user = self.spec.user.clone().ok_or_else(|| {
            VfsError::new(
                VfsErrorKind::Auth,
                format!(
                    "'{}' names no user — anonymous must be explicit (ftp://anonymous@host/…), never a fallback",
                    self.spec.display()
                ),
            )
        })?;
        let creds = self.creds.credentials_for(&self.spec)?;
        let password = if user == "anonymous" {
            creds
                .password
                .unwrap_or_else(|| "anonymous@syncdash".into())
        } else {
            creds.password.ok_or_else(|| {
                VfsError::new(
                    VfsErrorKind::Auth,
                    format!(
                        "no stored password for {user}@{} — store it with: syncdash cred set \"{}\"",
                        self.spec.host,
                        self.spec.display()
                    ),
                )
            })?
        };

        use std::net::ToSocketAddrs;
        let port = self.spec.port.unwrap_or(21);
        let addr = format!("{}:{}", self.spec.host, port)
            .to_socket_addrs()
            .map_err(|e| {
                VfsError::new(
                    VfsErrorKind::Transient,
                    format!("cannot resolve {}: {e}", self.spec.host),
                )
            })?
            .next()
            .ok_or_else(|| {
                VfsError::new(
                    VfsErrorKind::Transient,
                    format!("no address for {}", self.spec.host),
                )
            })?;

        // The TLS parameter is fixed at construction — `into_secure` performs AUTH TLS on a
        // stream that is *already* typed for it and hands back the same type, so the two schemes
        // branch here rather than upgrading one into the other. Securing happens before the login
        // below, which is the whole point: explicit FTPS, not the deprecated implicit kind on 990.
        let mut stream = if self.spec.scheme == "ftps" {
            let s = suppaftp::RustlsFtpStream::connect_timeout(addr, self.timeout)
                .map_err(|e| map_ftp_err("connect", e))?;
            Stream::Tls(Box::new(
                s.into_secure(tls_connector()?, &self.spec.host)
                    .map_err(|e| map_tls_err(&self.spec.host, port, e))?,
            ))
        } else {
            Stream::Plain(
                FtpStream::connect_timeout(addr, self.timeout)
                    .map_err(|e| map_ftp_err("connect", e))?,
            )
        };

        // FEAT before login: some servers only advertise honestly pre-auth; a refusal
        // just means an empty feature set (FFS: any FTP response = connectivity)
        let feats = match stream.feat() {
            Ok(map) => Feats {
                mlsd: map.contains_key("MLSD") || map.contains_key("MLST"),
                mfmt: map.contains_key("MFMT"),
                rest: map.contains_key("REST"),
            },
            Err(_) => Feats::default(),
        };
        if stream
            .feat()
            .map(|m| m.contains_key("UTF8"))
            .unwrap_or(false)
        {
            let _ = stream.opts("UTF8", Some("ON"));
        }

        stream.login(&user, &password).map_err(|e| {
            VfsError::new(
                VfsErrorKind::Auth,
                format!("login as {user}@{} refused: {e}", self.spec.host),
            )
        })?;
        stream
            .transfer_type(FileType::Binary)
            .map_err(|e| map_ftp_err("TYPE I", e))?;
        if self.spec.has_flag("active") {
            stream.set_mode(Mode::Active);
        }

        let _ = self.feats.set(feats);
        *guard = Some(FtpConn { stream });
        Ok(())
    }

    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        if rel.is_empty() {
            // Probing the root: a successful CWD is the directory's existence proof
            let abs = self.abs("");
            self.with_conn("stat root", |c| c.stream.cwd(&abs))?;
            return Ok(Some(VMeta {
                kind: VfsEntryKind::Directory,
                size: 0,
                mtime_ms: 0,
                mode: None,
                file_id: None,
                link: None,
            }));
        }
        // FTP has no stat: the parent listing IS the answer, which makes absence
        // self-confirming (the doctrine's requirement, satisfied by construction)
        let (parent, name) = crate::foundation::path::split_parent(rel);
        let parent = parent.trim_end_matches('/');
        let list = match self.list_dir(parent) {
            Ok(l) => l,
            Err(e) if e.kind == VfsErrorKind::NotFound => return Ok(None), // parent gone → child gone
            Err(e) => return Err(e),
        };
        Ok(list.iter().find(|f| f.name() == name).map(meta_of))
    }

    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        let mut out = Vec::new();
        for entry in self.list_dir(rel)? {
            let name = crate::foundation::path::EntryName::try_from(entry.name()).map_err(|e| {
                VfsError::new(
                    VfsErrorKind::Protocol,
                    format!("FTP server returned an invalid directory entry: {e}"),
                )
            })?;
            out.push(VDirEntry {
                name,
                meta: meta_of(&entry),
            });
        }
        Ok(out)
    }

    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        let abs = self.abs(rel);
        let data = self.with_conn("open_read", |c| c.stream.retr_as_stream(&abs))?;
        Ok(Box::new(FtpRead {
            conn: self.conn.clone(),
            data: Some(Box::new(data)),
            finished: false,
        }))
    }

    /// Refused, and `caps().ranged_read` says so before anyone asks.
    ///
    /// REST positions a transfer; what FTP has no clean answer for is *ending* one the client no
    /// longer wants. Verified against a live pyftpdlib: ABOR after the server's send had finished
    /// drew "225 No transfer to abort", and closing the data socket instead left the completion
    /// reply unread — either way a response stays queued on the control channel and the next
    /// command receives it. A `stat` came back "350 Restarting at position 0", the REST reply from
    /// two calls earlier. One control connection has nowhere to put that confusion, and every
    /// answer on it decides whether a file gets deleted.
    ///
    /// The window-fetching implementation is in git history at the commit that retired it. It is
    /// not kept here behind a flag: an unreachable partial implementation is the thing a later
    /// reader flips back on without knowing what it cost.
    fn read_range(&self, _rel: &str, _off: u64, _len: u32) -> VfsResult<Vec<u8>> {
        Err(VfsError::unsupported(
            "FTP cannot end a partial transfer without desynchronizing its control connection — \
             this backend reads whole files instead (caps.ranged_read = No)",
        ))
    }

    fn read_link(&self, rel: &str) -> VfsResult<String> {
        match self.stat(rel)? {
            Some(m) => m.link.ok_or_else(|| {
                VfsError::new(
                    VfsErrorKind::Io,
                    format!("not a symlink (or the listing carries no target): {rel}"),
                )
            }),
            None => Err(VfsError::new(
                VfsErrorKind::NotFound,
                format!("no such path: {rel}"),
            )),
        }
    }

    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        let mut prefix = String::new();
        for seg in rel.split('/').filter(|s| !s.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
            let abs = self.abs(&prefix);
            let made = self.with_conn("mkdir", |c| c.stream.mkdir(&abs));
            if let Err(e) = made {
                // Exists already? A CWD probe answers without parsing server prose
                let probe = self.with_conn("mkdir exists-probe", |c| c.stream.cwd(&abs));
                if probe.is_err() {
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        let (parent, base) = crate::foundation::path::split_parent(rel);
        let token = super::random_name_token()?;
        let tmp_rel = format!(
            "{parent}{}{base}.{token}",
            crate::foundation::names::TEMP_PREFIX,
        );
        let tmp_abs = self.abs(&tmp_rel);
        let dst_abs = self.abs(rel);
        let data = self.with_conn("open staged", |c| c.stream.put_with_stream(&tmp_abs))?;
        Ok(Box::new(FtpStaged {
            conn: self.conn.clone(),
            feats: self.feats.get().copied().unwrap_or_default(),
            tmp_abs,
            dst_abs,
            data: Some(Box::new(data)),
            wrote: 0,
            hint: hint.clone(),
            committed: false,
        }))
    }

    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        let (f, t) = (self.abs(from_rel), self.abs(to_rel));
        self.with_conn("rename", |c| c.stream.rename(&f, &t))
    }

    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        let abs = self.abs(rel);
        self.with_conn("remove_file", |c| c.stream.rm(&abs))
    }

    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        let abs = self.abs(rel);
        match self.with_conn("remove_dir", |c| c.stream.rmdir(&abs)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind != VfsErrorKind::Transient => {
                // Server prose varies; whether it is non-empty is checked by looking
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
        if !self.feats.get().map(|f| f.mfmt).unwrap_or(false) {
            return Err(VfsError::unsupported(
                "this server advertises no MFMT — mtimes cannot be set",
            ));
        }
        let abs = self.abs(rel);
        let stamp = crate::foundation::time::stamp_compact(mtime_ms).replace('-', "");
        self.with_conn("MFMT", |c| {
            c.stream
                .custom_command(format!("MFMT {stamp} {abs}"), &[suppaftp::Status::File])
                .map(|_| ())
        })
    }

    fn set_mode(&self, _rel: &str, _mode: u32) -> VfsResult<()> {
        Err(VfsError::unsupported("FTP carries no unix modes"))
    }

    fn make_symlink(&self, _rel: &str, _target: &str) -> VfsResult<()> {
        Err(VfsError::unsupported("FTP cannot create symlinks"))
    }

    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::spec::{parse, RootSpec};

    fn backend(s: &str) -> FtpBackend {
        let RootSpec::Endpoint(r) = parse(s) else {
            panic!()
        };
        FtpBackend::new(r, crate::fs::vfs::cred::default_provider())
    }

    #[test]
    fn caps_before_connect_assume_the_worst_honestly() {
        let b = backend("ftp://anonymous@nas/pub");
        let c = b.caps();
        assert_eq!(c.mtime_precision_ms, 60_000);
        assert_eq!(c.set_mtime, Support::Unknown);
        assert_eq!(c.fsync, Support::No);
        assert_eq!(c.max_parallel_streams, 1);
    }

    #[test]
    fn abs_paths_are_rooted() {
        let b = backend("ftp://u@h/pub/data");
        assert_eq!(b.abs(""), "/pub/data");
        assert_eq!(b.abs("x/y.txt"), "/pub/data/x/y.txt");
        let r = backend("ftp://u@h");
        assert_eq!(r.abs(""), "/");
        assert_eq!(r.abs("a.txt"), "/a.txt");
    }

    /// `ftps://` used to refuse outright — the root store was undecided and a half-secure default
    /// is worse than an honest no. Now it is a real scheme, so the thing to pin is that it goes
    /// down the *same* path as `ftp://` rather than a special one: a phrase with no stored secret
    /// fails on credentials, not on the scheme.
    #[test]
    fn ftps_is_a_real_scheme_and_authenticates_like_ftp() {
        let e = backend("ftps://u@h/x").connect().unwrap_err();
        assert_eq!(
            e.kind,
            VfsErrorKind::Auth,
            "no stored secret — same rung ftp:// stops at"
        );
        assert_ne!(
            e.kind,
            VfsErrorKind::Unsupported,
            "the scheme itself is supported now"
        );

        // And it is still refused when the phrase names nobody, for the same reason ftp:// is:
        // anonymous has to be asked for, never assumed.
        let e = backend("ftps://h/x").connect().unwrap_err();
        assert_eq!(e.kind, VfsErrorKind::Auth);
    }

    /// The trust store has to actually yield roots on this machine, or every `ftps://` connection
    /// fails at verification for a reason that looks like the server's fault.
    #[test]
    fn the_os_trust_store_yields_usable_roots() {
        assert!(
            tls_connector().is_ok(),
            "no roots readable from this machine's store — ftps:// could not verify anything"
        );
    }
}
