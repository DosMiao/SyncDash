//! The read stream and the staged write.
//!
//! Both hold the shared `ConnSlot` rather than borrowing the backend, so the connection can be
//! handed to a transfer while the backend stays usable. FTP's staging is the awkward one: the
//! server can only set an mtime after the rename, so `commit` reports what it managed rather
//! than failing the copy.

use std::io::{Read, Write};

use crate::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use crate::fs::vfs::{CommitReport, ReadStream, WriteHint, WriteStaged};


use super::{map_ftp_err, ConnSlot, Feats};

/// Blocking read over the FTP data connection. EOF finalizes the transfer on the
/// control connection; an early drop aborts it — either way the control channel is
/// left ready for the next command.
pub(super) struct FtpRead {
    pub(super) conn: ConnSlot,
    // The concrete DataStream type is unnameable (its TLS parameter is private);
    // finalize/abort take `impl Read`, so a box serves both reading and hand-back
    pub(super) data: Option<Box<dyn Read + Send>>,
    pub(super) finished: bool,
}

impl FtpRead {
    fn finish(&mut self, aborted: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(data) = self.data.take() {
            if let Some(conn) = self.conn.lock().unwrap().as_mut() {
                let _ = if aborted { conn.stream.abort(data) } else { conn.stream.finalize_retr_stream(data) };
            }
        }
    }
}

impl Read for FtpRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some(data) = self.data.as_mut() else {
            return Ok(0);
        };
        match data.read(buf) {
            Ok(0) => {
                self.finish(false);
                Ok(0)
            }
            other => other,
        }
    }
}

impl ReadStream for FtpRead {
    fn block_size(&self) -> usize {
        64 * 1024 // libcurl-era measurement: FTP data arrives in ~64 KiB strides
    }
}

impl Drop for FtpRead {
    fn drop(&mut self) {
        self.finish(true);
    }
}

pub(super) struct FtpStaged {
    pub(super) conn: ConnSlot,
    pub(super) feats: Feats,
    pub(super) tmp_abs: String,
    pub(super) dst_abs: String,
    pub(super) data: Option<Box<dyn Write + Send>>,
    pub(super) wrote: u64,
    pub(super) hint: WriteHint,
    pub(super) committed: bool,
}

impl WriteStaged for FtpStaged {
    fn write(&mut self, buf: &[u8]) -> VfsResult<()> {
        let data = self.data.as_mut().expect("write after seal");
        data.write_all(buf)
            .map_err(|e| VfsError::new(VfsErrorKind::Transient, format!("ftp upload write failed: {e}")))?;
        self.wrote += buf.len() as u64;
        Ok(())
    }

    fn block_size(&self) -> usize {
        64 * 1024
    }

    fn write_at(&mut self, _off: u64, _buf: &[u8]) -> VfsResult<()> {
        Err(VfsError::unsupported("FTP uploads are append-only streams"))
    }

    fn seal(&mut self, _fsync: bool) -> VfsResult<()> {
        // fsync does not exist in this protocol; caps say No and the preflight
        // NeedsAck line covered it — the flag is knowingly moot here
        if let Some(data) = self.data.take() {
            let mut guard = self.conn.lock().unwrap();
            let conn = guard
                .as_mut()
                .ok_or_else(|| VfsError::new(VfsErrorKind::Transient, "connection lost before seal"))?;
            conn.stream
                .finalize_put_stream(data)
                .map_err(|e| map_ftp_err("finalize upload", e))?;
        }
        Ok(())
    }

    fn staged_len(&self) -> VfsResult<u64> {
        let mut guard = self.conn.lock().unwrap();
        let conn = guard
            .as_mut()
            .ok_or_else(|| VfsError::new(VfsErrorKind::Transient, "connection lost"))?;
        conn.stream
            .size(&self.tmp_abs)
            .map(|s| s as u64)
            .map_err(|e| map_ftp_err("SIZE staged", e))
    }

    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>> {
        // Read-back on FTP is a full re-download of the staged file — verify's honest
        // price on this backend
        let mut guard = self.conn.lock().unwrap();
        let conn = guard
            .as_mut()
            .ok_or_else(|| VfsError::new(VfsErrorKind::Transient, "connection lost"))?;
        let data = conn
            .stream
            .retr_as_stream(&self.tmp_abs)
            .map_err(|e| map_ftp_err("open staged for read-back", e))?;
        Ok(Box::new(FtpRead { conn: self.conn.clone(), data: Some(Box::new(data)), finished: false }))
    }

    fn commit(mut self: Box<Self>) -> VfsResult<CommitReport> {
        if self.data.is_some() {
            self.seal(false)?;
        }
        {
            let mut guard = self.conn.lock().unwrap();
            let conn = guard
                .as_mut()
                .ok_or_else(|| VfsError::new(VfsErrorKind::Transient, "connection lost before commit"))?;
            // Clear the destination (server rename-overwrite behavior varies — never rely on it)
            match conn.stream.rm(&self.dst_abs) {
                Ok(()) => {}
                Err(e) => {
                    let v = map_ftp_err("clear destination", e);
                    if v.kind != VfsErrorKind::NotFound {
                        return Err(v);
                    }
                }
            }
            conn.stream
                .rename(&self.tmp_abs, &self.dst_abs)
                .map_err(|e| map_ftp_err("rename into place", e))?;
            // Write-after confirmation: the landed size must match what we sent
            let landed = conn
                .stream
                .size(&self.dst_abs)
                .map_err(|e| map_ftp_err("SIZE after rename", e))? as u64;
            if landed != self.wrote {
                return Err(VfsError::new(
                    VfsErrorKind::Protocol,
                    format!("after rename the server holds {landed} bytes but the upload carried {}", self.wrote),
                ));
            }
        }
        self.committed = true;

        let mut report = CommitReport::default();
        if let Some(ms) = self.hint.mtime_ms {
            if self.feats.mfmt {
                let stamp = crate::foundation::time::stamp_compact(ms).replace('-', "");
                let mut guard = self.conn.lock().unwrap();
                if let Some(conn) = guard.as_mut() {
                    match conn
                        .stream
                        .custom_command(format!("MFMT {stamp} {}", self.dst_abs), &[suppaftp::Status::File])
                    {
                        Ok(_) => report.mtime_ondisk_ms = Some((ms / 1000) * 1000), // second-granular, as accepted
                        Err(e) => report.mtime_error = Some(map_ftp_err("MFMT", e)),
                    }
                }
            } else {
                report.mtime_error = Some(VfsError::unsupported(
                    "no MFMT — the file keeps the server's own timestamp (the correction table absorbs this for compare)",
                ));
            }
        }
        Ok(report)
    }
}

impl Drop for FtpStaged {
    fn drop(&mut self) {
        if !self.committed {
            // Close the data connection (the server finishes the partial STOR), then
            // delete the temp — the destination was never touched
            if let Some(data) = self.data.take() {
                if let Some(conn) = self.conn.lock().unwrap().as_mut() {
                    let _ = conn.stream.finalize_put_stream(data);
                }
            }
            if let Some(conn) = self.conn.lock().unwrap().as_mut() {
                let _ = conn.stream.rm(&self.tmp_abs);
            }
        }
    }
}
