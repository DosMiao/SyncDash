//! The read stream and the staged write on a native SMB root.
//!
//! Both own a clone of the session's `Connection` rather than borrowing the backend, which is
//! what lets a copy run on a worker thread while the backend keeps answering `stat` on another.
//!
//! Why the write path issues its own CREATE and WRITEs instead of taking the crate's
//! `FileWriter`: that writer opens with `ShareAccess(0)`, so nothing else may touch the file
//! while it lives, and its `bytes_written()` counts only responses it has drained — a pipelined
//! write that is still in flight reads as zero. Between them those two make an honest
//! `staged_len` impossible before the handle closes, and `staged_len` is the finalize check
//! that catches a truncated transfer before it becomes the destination. Opening the handle
//! here with sharing allowed buys a real server-side size at any point, and explicit control
//! over when the FLUSH goes out.

use std::sync::Arc;
use std::time::Duration;

use crate::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use crate::fs::vfs::{CommitReport, ReadStream, WriteHint, WriteStaged};

use smb2::client::connection::Connection;
use smb2::client::stream::FileReader;
use smb2::msg::flush::FlushRequest;
use smb2::msg::write::{WriteRequest, WriteResponse};
use smb2::pack::{ReadCursor, Unpack};
use smb2::types::status::NtStatus;
use smb2::types::{Command, CreditCharge, FileId};
use smb2::Tree;

use super::{basic_info, map_smb_err, set_write_time, status_err};

/// One SMB2 credit covers 64 KiB; a larger payload has to charge — and consume — one per
/// 64 KiB, because a gap in the message-id sequence makes the server drop the connection.
const BYTES_PER_CREDIT: u64 = 65_536;

/// Where the write data sits relative to the SMB2 header: 64-byte header plus the 48-byte
/// fixed part of the WRITE request. The crate hard-codes the same value.
const WRITE_DATA_OFFSET: u16 = 0x70;

/// What the engine's copy loop reads and writes in one go. SMB2 negotiates a maximum write
/// of 1 MiB or more on every dialect this client speaks, so a 1 MiB block is one round-trip.
const BLOCK: usize = 1024 * 1024;

/// Run one async step under a timeout. A timeout is connection trouble, never a statement
/// about a file.
fn block_on<F, T>(
    rt: &tokio::runtime::Handle,
    timeout: Duration,
    what: &str,
    fut: F,
) -> VfsResult<T>
where
    F: std::future::Future<Output = VfsResult<T>>,
{
    match rt.block_on(async { tokio::time::timeout(timeout, fut).await }) {
        Ok(r) => r,
        Err(_) => Err(VfsError::new(
            VfsErrorKind::Transient,
            format!("{what} timed out after {timeout:?}"),
        )),
    }
}

/// Blocking `Read` over the positioned SMB reader: the engine's hashing loops read through
/// this without knowing an event loop exists.
pub(super) struct SmbRead {
    rt: tokio::runtime::Handle,
    timeout: Duration,
    /// `None` only after `close`; `FileReader::close` consumes itself, and a reader dropped
    /// without it leaks the handle on the server.
    reader: Option<FileReader>,
    pos: u64,
}

impl SmbRead {
    pub(super) fn new(
        rt: tokio::runtime::Handle,
        timeout: Duration,
        reader: FileReader,
    ) -> SmbRead {
        SmbRead {
            rt,
            timeout,
            reader: Some(reader),
            pos: 0,
        }
    }
}

impl std::io::Read for SmbRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some(reader) = self.reader.as_ref() else {
            return Ok(0);
        };
        let (pos, len) = (self.pos, buf.len() as u64);
        let got = block_on(&self.rt, self.timeout, "read", async move {
            reader
                .read_at(pos, len)
                .await
                .map_err(|e| map_smb_err("read", e))
        })?;
        buf[..got.len()].copy_from_slice(&got);
        self.pos += got.len() as u64;
        Ok(got.len())
    }
}

impl ReadStream for SmbRead {
    fn block_size(&self) -> usize {
        BLOCK
    }
}

impl Drop for SmbRead {
    fn drop(&mut self) {
        if let Some(r) = self.reader.take() {
            let _ = block_on(&self.rt, self.timeout, "close read handle", async move {
                r.close().await.map_err(|e| map_smb_err("close", e))
            });
        }
    }
}

/// The staged write: a temp name in the destination's own directory (so the landing rename
/// stays inside one share), written through a handle that permits sharing, sealed by FLUSH +
/// CLOSE, and landed with `ReplaceIfExists = 0`. Ordinary commit clears the destination first;
/// no-replace commit relies on the rename refusal. mtime goes on by path after the rename because
/// a server may replace an open handle's write time when it closes.
pub(super) struct SmbStaged {
    pub(super) rt: tokio::runtime::Handle,
    pub(super) timeout: Duration,
    pub(super) tree: Arc<Tree>,
    pub(super) conn: Connection,
    pub(super) tmp_share_rel: String,
    pub(super) dst_share_rel: String,
    /// The destination in wire form, for the hand-rolled SET_INFO compound at commit.
    pub(super) dst_wire_path: String,
    pub(super) max_write: u32,
    /// `None` once sealed.
    pub(super) file_id: Option<FileId>,
    pub(super) offset: u64,
    pub(super) hint: WriteHint,
    pub(super) committed: bool,
}

impl SmbStaged {
    fn block<F, T>(&self, what: &str, fut: F) -> VfsResult<T>
    where
        F: std::future::Future<Output = VfsResult<T>>,
    {
        block_on(&self.rt, self.timeout, what, fut)
    }

    /// Close the write handle if it is still open. Idempotent.
    fn close_handle(&mut self) -> VfsResult<()> {
        let Some(file_id) = self.file_id.take() else {
            return Ok(());
        };
        let (tree, mut conn) = (self.tree.clone(), self.conn.clone());
        self.block("close staged handle", async move {
            tree.close_handle(&mut conn, file_id)
                .await
                .map_err(|e| map_smb_err("close", e))
        })
    }

    fn commit_inner(&mut self, replace: bool) -> VfsResult<CommitReport> {
        self.seal(false)?;
        if replace {
            let (tree, mut conn) = (self.tree.clone(), self.conn.clone());
            let destination = self.dst_share_rel.clone();
            match self.block("clear destination", async move {
                tree.delete_file(&mut conn, &destination)
                    .await
                    .map_err(|e| map_smb_err("clear destination", e))
            }) {
                Ok(()) => {}
                Err(e) if e.kind == VfsErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let (tree, mut conn) = (self.tree.clone(), self.conn.clone());
        let (temp, destination) = (self.tmp_share_rel.clone(), self.dst_share_rel.clone());
        self.block("rename into place", async move {
            tree.rename(&mut conn, &temp, &destination)
                .await
                .map_err(|e| map_smb_err("rename into place", e))
        })?;
        self.committed = true;

        let mut report = CommitReport::default();
        if let Some(ms) = self.hint.mtime_ms {
            let (tree, conn) = (self.tree.clone(), self.conn.clone());
            let path = self.dst_wire_path.clone();
            let ticks = basic_info::filetime_from_unix_ms(ms);
            match self.block("set mtime after rename", async move {
                set_write_time(&tree, &conn, &path, ticks).await
            }) {
                Ok(()) => {
                    let (tree, mut conn) = (self.tree.clone(), self.conn.clone());
                    let destination = self.dst_share_rel.clone();
                    if let Ok(info) = self.block("stat back", async move {
                        tree.stat(&mut conn, &destination)
                            .await
                            .map_err(|e| map_smb_err("stat back", e))
                    }) {
                        report.mtime_ondisk_ms =
                            Some(basic_info::unix_ms_from_filetime(info.modified.0));
                    }
                }
                Err(e) => report.mtime_error = Some(e),
            }
        }
        Ok(report)
    }
}

impl WriteStaged for SmbStaged {
    fn write(&mut self, buf: &[u8]) -> VfsResult<()> {
        let Some(file_id) = self.file_id else {
            return Err(VfsError::new(
                VfsErrorKind::Io,
                "write after seal on a staged smb file",
            ));
        };
        let chunk_max = self.max_write.max(1) as usize;
        for chunk in buf.chunks(chunk_max) {
            let offset = self.offset;
            let len = chunk.len() as u64;
            let req = WriteRequest {
                data_offset: WRITE_DATA_OFFSET,
                offset,
                file_id,
                channel: 0,
                remaining_bytes: 0,
                write_channel_info_offset: 0,
                write_channel_info_length: 0,
                flags: 0,
                data: chunk.to_vec(),
            };
            let charge = CreditCharge(len.div_ceil(BYTES_PER_CREDIT).max(1) as u16);
            let tree_id = self.tree.tree_id;
            let conn = &self.conn;
            let frame = self.block("write", async move {
                conn.execute_with_credits(Command::Write, &req, Some(tree_id), charge)
                    .await
                    .map_err(|e| map_smb_err("write", e))
            })?;
            if frame.header.status != NtStatus::SUCCESS {
                return Err(status_err("write", Command::Write, frame.header.status));
            }
            let resp = WriteResponse::unpack(&mut ReadCursor::new(&frame.body))
                .map_err(|e| map_smb_err("write (response)", e))?;
            // A server that acknowledges fewer bytes than went out has silently truncated the
            // file. Accepting it here is how a short copy reaches the destination looking whole.
            if u64::from(resp.count) != len {
                return Err(VfsError::new(
                    VfsErrorKind::Protocol,
                    format!(
                        "the server acknowledged {} of {len} bytes at offset {offset} of '{}'",
                        resp.count, self.tmp_share_rel
                    ),
                ));
            }
            self.offset += len;
        }
        Ok(())
    }

    fn block_size(&self) -> usize {
        BLOCK
    }

    fn write_at(&mut self, _off: u64, _buf: &[u8]) -> VfsResult<()> {
        Err(VfsError::unsupported(
            "random-access writes are not offered on smb roots (caps().write_at says No; delta is a both-local affair)",
        ))
    }

    fn seal(&mut self, fsync: bool) -> VfsResult<()> {
        let Some(file_id) = self.file_id else {
            return Ok(()); // already sealed
        };
        if fsync {
            let tree_id = self.tree.tree_id;
            let conn = &self.conn;
            let req = FlushRequest { file_id };
            let frame = self.block("fsync", async move {
                conn.execute(Command::Flush, &req, Some(tree_id))
                    .await
                    .map_err(|e| map_smb_err("fsync", e))
            })?;
            if frame.header.status != NtStatus::SUCCESS {
                // Asked for and not delivered: per the preflight agreement that fails the file
                // rather than landing bytes the server has not promised to keep.
                return Err(status_err("fsync", Command::Flush, frame.header.status));
            }
        }
        self.close_handle()
    }

    fn staged_len(&self) -> VfsResult<u64> {
        // The server's own answer, not our count of what we sent — which is the entire point
        // of the reconciliation. Readable before the seal as well as after, because the handle
        // this backend opens permits sharing.
        let (tree, mut conn) = (self.tree.clone(), self.conn.clone());
        let p = self.tmp_share_rel.clone();
        let info = self.block("staged_len", async move {
            tree.stat(&mut conn, &p)
                .await
                .map_err(|e| map_smb_err("staged_len", e))
        })?;
        Ok(info.size)
    }

    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>> {
        let (tree, conn) = (self.tree.clone(), self.conn.clone());
        let p = self.tmp_share_rel.clone();
        let reader = self.block("open staged for read-back", async move {
            smb2::client::stream::open_file_reader(tree, conn, &p)
                .await
                .map_err(|e| map_smb_err("open staged for read-back", e))
        })?;
        Ok(Box::new(SmbRead::new(
            self.rt.clone(),
            self.timeout,
            reader,
        )))
    }

    fn commit(mut self: Box<Self>) -> VfsResult<CommitReport> {
        self.commit_inner(true)
    }

    fn commit_noreplace(mut self: Box<Self>) -> VfsResult<CommitReport> {
        self.commit_inner(false)
    }
}

impl Drop for SmbStaged {
    /// An interruption anywhere must leave no debris and an untouched destination.
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = self.close_handle();
        let (tree, mut conn) = (self.tree.clone(), self.conn.clone());
        let p = self.tmp_share_rel.clone();
        let _ = self.block("remove abandoned staged file", async move {
            tree.delete_file(&mut conn, &p)
                .await
                .map_err(|e| map_smb_err("remove staged", e))
        });
    }
}
