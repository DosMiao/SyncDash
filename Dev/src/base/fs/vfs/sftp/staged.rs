//! The read stream and the staged write.
//!
//! Both hold their own handle to the session rather than borrowing the backend, which is what
//! makes them separable — and what lets a copy run on a worker thread while the backend keeps
//! answering `stat` on another.

use std::sync::Arc;
use std::time::Duration;

use crate::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use crate::fs::vfs::{CommitReport, ReadStream, WriteHint, WriteStaged};

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::FileAttributes;

use super::{attrs_none, map_sftp_err};

/// Blocking `Read` over the async sftp file: the engine's hashing loops read through
/// this without knowing an event loop exists.
pub(super) struct SftpRead {
    pub(super) rt: tokio::runtime::Handle,
    pub(super) timeout: Duration,
    pub(super) file: russh_sftp::client::fs::File,
}

impl std::io::Read for SftpRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use tokio::io::AsyncReadExt;
        let d = self.timeout;
        let file = &mut self.file;
        match self
            .rt
            .clone()
            .block_on(async { tokio::time::timeout(d, file.read(buf)).await })
        {
            Ok(r) => r,
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "sftp read timed out",
            )),
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
/// O_EXCL refuses collisions. Ordinary commit clears the destination before rename;
/// no-replace commit requires OpenSSH's hard-link extension, whose link creation is the
/// cross-session atomic publish primitive. A server without it is rejected during preflight and
/// fails closed here as a second line of defense. mtime and mode go on by path after publication,
/// with failures carried in `CommitReport`.
pub(super) struct SftpStaged {
    pub(super) rt: tokio::runtime::Handle,
    pub(super) timeout: Duration,
    pub(super) sftp: Arc<SftpSession>,
    pub(super) tmp_abs: String,
    pub(super) dst_abs: String,
    pub(super) file: Option<russh_sftp::client::fs::File>,
    pub(super) hint: WriteHint,
    pub(super) committed: bool,
}

impl SftpStaged {
    fn block<F, T>(&self, what: &str, fut: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = Result<T, russh_sftp::client::error::Error>>,
    {
        crate::fs::vfs::block_with_timeout(&self.rt, self.timeout, what, None, async move {
            fut.await.map_err(|e| map_sftp_err(what, e))
        })
    }

    fn commit_inner(&mut self, replace: bool) -> VfsResult<CommitReport> {
        if self.file.is_some() {
            self.seal(false)?;
        }
        if replace {
            let s = self.sftp.clone();
            let dst = self.dst_abs.clone();
            match self.block("clear destination", async move { s.remove_file(dst).await }) {
                Ok(()) => {}
                Err(e) if e.kind == VfsErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            let s = self.sftp.clone();
            let (temp, destination) = (self.tmp_abs.clone(), self.dst_abs.clone());
            self.block("rename into place", async move {
                s.rename(temp, destination).await
            })?;
            self.committed = true;
        } else {
            let s = self.sftp.clone();
            let (temp, destination) = (self.tmp_abs.clone(), self.dst_abs.clone());
            let hardlinked = self.block("publish without replacement", async move {
                s.hardlink(temp, destination).await
            })?;
            if hardlinked {
                // The destination is durable as soon as the link succeeds. Mark the commit before
                // best-effort temp cleanup so a cleanup failure cannot make Drop remove published
                // data or make a lock caller report a successful exclusive acquisition as failed.
                self.committed = true;
                let s = self.sftp.clone();
                let temp = self.tmp_abs.clone();
                if let Err(error) = self.block("remove linked staged file", async move {
                    s.remove_file(temp).await
                }) {
                    crate::log_warn!(
                        "sftp",
                        "sftp no-replace publication left a filtered temp file for later cleanup: {}",
                        error
                    );
                }
            } else {
                return Err(VfsError::unsupported(
                    "atomic no-replace publication requires hardlink@openssh.com",
                ));
            }
        }

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
            if let Err(e) = self.block("setstat after publication", async move {
                s.set_metadata(dst, attrs).await
            }) {
                if self.hint.mode.is_some() {
                    report.mode_error = Some(e);
                } else {
                    report.mtime_error = Some(e);
                }
            } else if self.hint.mtime_ms.is_some() {
                let s = self.sftp.clone();
                let dst = self.dst_abs.clone();
                if let Ok(attributes) =
                    self.block("stat back", async move { s.symlink_metadata(dst).await })
                {
                    report.mtime_ondisk_ms = attributes.mtime.map(|seconds| seconds as i64 * 1000);
                }
            }
        }
        Ok(report)
    }
}

impl WriteStaged for SftpStaged {
    fn write(&mut self, buf: &[u8]) -> VfsResult<()> {
        use tokio::io::AsyncWriteExt;
        let d = self.timeout;
        let f = self.file.as_mut().expect("write after seal");
        match self
            .rt
            .block_on(async { tokio::time::timeout(d, f.write_all(buf)).await })
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("sftp write failed: {e}"),
            )),
            Err(_) => Err(VfsError::new(
                VfsErrorKind::Transient,
                "sftp write timed out",
            )),
        }
    }

    fn block_size(&self) -> usize {
        480 * 1024
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
                        f.sync_all().await.map_err(std::io::Error::other)?;
                    }
                    f.shutdown().await
                })
                .await
            });
            match res {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(VfsError::new(
                    if fsync {
                        VfsErrorKind::Protocol
                    } else {
                        VfsErrorKind::Transient
                    },
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
        Ok(Box::new(SftpRead {
            rt: self.rt.clone(),
            timeout: self.timeout,
            file,
        }))
    }

    fn commit(mut self: Box<Self>) -> VfsResult<CommitReport> {
        self.commit_inner(true)
    }

    fn commit_noreplace(mut self: Box<Self>) -> VfsResult<CommitReport> {
        self.commit_inner(false)
    }
}

impl Drop for SftpStaged {
    fn drop(&mut self) {
        if !self.committed {
            use tokio::io::AsyncWriteExt;
            let d = self.timeout;
            if let Some(mut f) = self.file.take() {
                let _ = self
                    .rt
                    .block_on(async { tokio::time::timeout(d, f.shutdown()).await });
            }
            let s = self.sftp.clone();
            let t = self.tmp_abs.clone();
            let _ = self.rt.block_on(async {
                tokio::time::timeout(d, async move { s.remove_file(t).await }).await
            });
        }
    }
}
