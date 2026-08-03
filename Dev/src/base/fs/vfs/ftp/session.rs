//! Connecting, authenticating, and holding the FTP session.

use super::super::error::{VfsError, VfsErrorKind, VfsResult};
use super::super::spec::EndpointSpec;
use super::stream::*;
use super::tls::*;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use suppaftp::list::File as FtpFile;
use suppaftp::FtpError;

pub struct FtpBackend {
    pub(super) spec: EndpointSpec,
    pub(super) timeout: Duration,
    pub(super) conn: ConnSlot,
    pub(super) feats: OnceLock<Feats>,
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
    pub fn new(spec: EndpointSpec) -> FtpBackend {
        let timeout = spec.timeout();
        FtpBackend {
            spec,
            timeout,
            conn: Arc::new(Mutex::new(None)),
            feats: OnceLock::new(),
        }
    }

    pub(super) fn abs(&self, rel: &str) -> String {
        let root = self.spec.root.trim_matches('/');
        match (root.is_empty(), rel.is_empty()) {
            (true, true) => "/".into(),
            (true, false) => format!("/{rel}"),
            (false, true) => format!("/{root}"),
            (false, false) => format!("/{root}/{rel}"),
        }
    }

    pub(super) fn with_conn<T>(
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
    pub(super) fn list_dir(&self, rel: &str) -> VfsResult<Vec<FtpFile>> {
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
