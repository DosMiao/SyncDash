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

mod meta;
mod session;
mod staged;
mod stream;
mod tls;

#[cfg(test)]
mod tests;

pub use session::FtpBackend;

use self::meta::meta_of;
use self::session::*;
use self::staged::{FtpRead, FtpStaged};
use self::stream::*;
use self::tls::*;
use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::VfsEntryKind;
use super::{
    CaseSense, ReadStream, Support, VDirEntry, VMeta, Vfs, VfsCaps, WriteHint, WriteStaged,
};
use suppaftp::types::FileType;
use suppaftp::{FtpStream, Mode};

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
