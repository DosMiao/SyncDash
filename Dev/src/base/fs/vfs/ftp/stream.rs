//! The control connection, plain or TLS-wrapped, behind one type.
//!
//! FTP and FTPS differ only in whether the stream is upgraded, so every operation above this layer
//! is written once. The variants are kept in one enum rather than behind a trait because the
//! suppaftp client's methods take `&mut self` and a trait object would force a second lock.

use std::io::Read;
use suppaftp::types::FileType;
use suppaftp::{FtpError, FtpStream, Mode};

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
