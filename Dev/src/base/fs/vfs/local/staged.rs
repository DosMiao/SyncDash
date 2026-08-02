//! Reading and staged writing against a retained root handle.
//!
//! Named to match the sibling backends: `smb/staged.rs`, `ftp/staged.rs`, `sftp/staged.rs` all own
//! the same responsibility for their protocol. A staged write is published by rename, so a reader
//! sees either the previous content or the complete new content and never a partial file.

use super::super::error::VfsResult;
use super::super::{CommitReport, ReadStream, WriteHint, WriteStaged};
use super::meta::metadata_mtime_ms;
use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::{LocalRoot, LocalStagedFile};

pub(super) struct LocalRead {
    file: std::fs::File,
}

impl std::io::Read for LocalRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }
}

impl ReadStream for LocalRead {
    fn block_size(&self) -> usize {
        1024 * 1024
    }
}
pub(super) struct LocalStaged {
    root: LocalRoot,
    staged: Option<LocalStagedFile>,
    destination: RootRelativePath,
    hint: WriteHint,
    sync_requested: bool,
}

impl LocalRead {
    pub(super) fn new(file: std::fs::File) -> Self {
        Self { file }
    }
}

impl LocalStaged {
    pub(super) fn new(
        root: LocalRoot,
        staged: LocalStagedFile,
        destination: RootRelativePath,
        hint: WriteHint,
    ) -> Self {
        Self {
            root,
            staged: Some(staged),
            destination,
            hint,
            // Set only by an explicit `sync()` from the caller; a staged write is durable on
            // commit regardless, so defaulting this true would fsync every file.
            sync_requested: false,
        }
    }

    pub(super) fn commit_with(&mut self, replace: bool) -> VfsResult<CommitReport> {
        let staged = self.staged.take().expect("double commit");
        let mut report = CommitReport::default();

        if let Some(ms) = self.hint.mtime_ms {
            if let Err(error) = staged.set_mtime(ms) {
                report.mtime_error = Some(error.into());
            }
        }
        #[cfg(unix)]
        if let Some(mode) = self.hint.mode {
            if let Err(error) = staged.set_mode(mode) {
                report.mode_error = Some(error.into());
            }
        }
        if self.sync_requested
            && (self.hint.mtime_ms.is_some() || (cfg!(unix) && self.hint.mode.is_some()))
        {
            staged.sync_file()?;
        }

        if replace {
            staged.commit()?;
        } else {
            staged.commit_noreplace()?;
        }

        if self.hint.mtime_ms.is_some() {
            report.mtime_ondisk_ms = self
                .root
                .metadata_path(&self.destination)
                .ok()
                .map(|metadata| metadata_mtime_ms(&metadata));
        }
        Ok(report)
    }
}

impl WriteStaged for LocalStaged {
    fn write(&mut self, buf: &[u8]) -> VfsResult<()> {
        let s = self.staged.as_mut().expect("write after commit");
        s.write_all_from(&mut &buf[..])?;
        Ok(())
    }

    fn block_size(&self) -> usize {
        1024 * 1024
    }

    fn write_at(&mut self, off: u64, buf: &[u8]) -> VfsResult<()> {
        let s = self.staged.as_mut().expect("write after commit");
        s.write_at(off, buf)?;
        Ok(())
    }

    fn seal(&mut self, fsync: bool) -> VfsResult<()> {
        let s = self.staged.as_mut().expect("seal after commit");
        self.sync_requested |= fsync;
        s.seal(fsync)?;
        Ok(())
    }

    fn staged_len(&self) -> VfsResult<u64> {
        let s = self.staged.as_ref().expect("staged_len after commit");
        Ok(s.staged_len()?)
    }

    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>> {
        let s = self.staged.as_ref().expect("read after commit");
        Ok(Box::new(LocalRead {
            file: s.try_clone_file()?,
        }))
    }

    fn commit(mut self: Box<Self>) -> VfsResult<CommitReport> {
        self.commit_with(true)
    }

    fn commit_noreplace(mut self: Box<Self>) -> VfsResult<CommitReport> {
        self.commit_with(false)
    }
}
