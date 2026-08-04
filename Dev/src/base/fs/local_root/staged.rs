//! Staged-file lifecycle: write, seal, and one-entry commit.

use std::io::{Read, Seek, Write};

use crate::foundation::names::TEMP_PREFIX;
use crate::foundation::path::EntryName;

use super::identity::{random_token, EntryNameOsStr};
use super::{LocalDirectory, LocalStagedFile};

impl LocalStagedFile {
    pub(super) fn create(
        parent: LocalDirectory,
        destination_name: EntryName,
    ) -> std::io::Result<Self> {
        loop {
            let temporary_name =
                EntryName::try_from(format!("{TEMP_PREFIX}stage.{}", random_token()?))
                    .expect("generated staging names satisfy the entry-name contract");
            match parent.create_file(temporary_name.as_os_str()) {
                Ok(file) => {
                    return Ok(Self {
                        parent,
                        temporary_name,
                        destination_name,
                        file: Some(file),
                        committed: false,
                        sync_parent_on_commit: false,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub fn staged_len(&self) -> std::io::Result<u64> {
        match &self.file {
            Some(file) => Ok(file.metadata()?.len()),
            None => Ok(self.parent.metadata(self.temporary_name.as_os_str())?.len()),
        }
    }

    pub fn try_clone_file(&self) -> std::io::Result<std::fs::File> {
        match &self.file {
            Some(file) => file.try_clone(),
            None => self.parent.open_read(self.temporary_name.as_os_str()),
        }
    }

    pub fn set_mtime(&self, mtime_ms: i64) -> std::io::Result<()> {
        match &self.file {
            Some(file) => {
                let time = filetime::FileTime::from_unix_time(
                    mtime_ms.div_euclid(1000),
                    (mtime_ms.rem_euclid(1000) * 1_000_000) as u32,
                );
                filetime::set_file_handle_times(file, None, Some(time))
            }
            None => self
                .parent
                .set_mtime(self.temporary_name.as_os_str(), mtime_ms),
        }
    }

    #[cfg(unix)]
    pub fn set_mode(&self, mode: u32) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let file = self.try_clone_file()?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
    }

    pub fn write_all_from(&mut self, reader: &mut dyn Read) -> std::io::Result<u64> {
        std::io::copy(reader, self.file_mut()?)
    }

    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
        let file = self.file_mut()?;
        file.seek(std::io::SeekFrom::Start(offset))?;
        file.write_all(bytes)
    }

    pub fn set_len(&mut self, length: u64) -> std::io::Result<()> {
        self.file_mut()?.set_len(length)
    }

    pub fn seal(&mut self, fsync: bool) -> std::io::Result<()> {
        self.sync_parent_on_commit |= fsync;
        if let Some(mut file) = self.file.take() {
            file.flush()?;
            if fsync {
                file.sync_all()?;
            }
        }
        Ok(())
    }

    #[cfg(not(windows))]
    pub fn sync_file(&self) -> std::io::Result<()> {
        self.try_clone_file()?.sync_all()
    }

    /// The sealed reopen cannot share [`Self::try_clone_file`]'s read-only lane here:
    /// `FlushFileBuffers` demands write access (denied through a read handle, measured on Win11
    /// 26200). Unix keeps the read-only reopen because a staged payload may already carry a
    /// read-only mode, and fsync through `O_RDONLY` is valid there; on Windows `set_mode` does
    /// not exist, so the temporary stays writable until commit.
    #[cfg(windows)]
    pub fn sync_file(&self) -> std::io::Result<()> {
        match &self.file {
            Some(file) => file.sync_all(),
            None => self
                .parent
                .open_read_write(self.temporary_name.as_os_str())?
                .sync_all(),
        }
    }

    pub fn commit(mut self) -> std::io::Result<()> {
        if self.file.is_some() {
            self.seal(true)?;
        }
        self.parent.rename(
            self.temporary_name.as_os_str(),
            &self.parent,
            self.destination_name.as_os_str(),
        )?;
        self.committed = true;
        if self.sync_parent_on_commit {
            self.parent.sync_all()?;
        }
        Ok(())
    }

    pub fn commit_noreplace(mut self) -> std::io::Result<()> {
        if self.file.is_some() {
            self.seal(true)?;
        }
        self.parent.rename_noreplace(
            self.temporary_name.as_os_str(),
            &self.parent,
            self.destination_name.as_os_str(),
        )?;
        self.committed = true;
        if self.sync_parent_on_commit {
            self.parent.sync_all()?;
        }
        Ok(())
    }

    fn file_mut(&mut self) -> std::io::Result<&mut std::fs::File> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file is already sealed"))
    }
}

impl Write for LocalStagedFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file is already sealed"))?
            .write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file is already sealed"))?
            .flush()
    }
}

impl Drop for LocalStagedFile {
    fn drop(&mut self) {
        if !self.committed {
            self.file.take();
            let _ = self.parent.remove_file(self.temporary_name.as_os_str());
        }
    }
}
