//! The local backend: std::fs + the existing `fs::staged::Staged`, nothing reinvented.
//!
//! `as_local()` returns the root, which routes scan down the existing
//! walkdir+mmap fast path — this impl is what the *generic* engine lanes use, and
//! its write side deliberately wraps the very same `Staged` the direct lane uses,
//! so local behaviour cannot drift between lanes.

use std::path::{Path, PathBuf};

use super::error::VfsResult;
use super::{
    CaseSense, CommitReport, EntryKind, ReadStream, Support, VDirEntry, VMeta, Vfs, VfsCaps,
    WriteHint, WriteStaged,
};
use crate::foundation::path::join_native;
use crate::fs::staged::Staged;

pub struct LocalVfs {
    root: PathBuf,
    /// The root exactly as the job spelled it — `identity()` must reproduce it so
    /// existing hash-cache / mtime-fix files keep their names.
    root_str: String,
}

impl LocalVfs {
    pub fn new(root: PathBuf) -> LocalVfs {
        let root_str = root.to_string_lossy().into_owned();
        LocalVfs { root, root_str }
    }

    fn abs(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            join_native(&self.root, rel)
        }
    }
}

fn meta_of(md: &std::fs::Metadata) -> VMeta {
    let kind = if md.file_type().is_symlink() {
        EntryKind::Symlink
    } else if md.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    VMeta {
        kind,
        size: md.len(),
        mtime_ms: crate::foundation::time::meta_mtime_ms(md),
        mode: mode_of(md),
        file_id: file_id_of(md),
        link: None, // lazy: read_link on demand
    }
}

#[cfg(unix)]
fn mode_of(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(md.mode() & 0o7777)
}
#[cfg(not(unix))]
fn mode_of(_md: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn file_id_of(md: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("{}:{}", md.dev(), md.ino()))
}
#[cfg(not(unix))]
fn file_id_of(_md: &std::fs::Metadata) -> Option<String> {
    None
}

struct LocalRead {
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
use std::io::Read as _;

/// `Staged` plus the write hint: mtime/mode land on the temp file (pre-rename, the
/// same order the direct apply lane uses), the post-rename stat feeds the
/// mtime-correction table.
struct LocalStaged {
    staged: Option<Staged>,
    dst: PathBuf,
    hint: WriteHint,
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
        s.seal(fsync)?;
        Ok(())
    }

    fn staged_len(&self) -> VfsResult<u64> {
        let s = self.staged.as_ref().expect("staged_len after commit");
        Ok(std::fs::metadata(s.path())?.len())
    }

    fn open_staged_read(&self) -> VfsResult<Box<dyn ReadStream>> {
        let s = self.staged.as_ref().expect("read after commit");
        Ok(Box::new(LocalRead { file: std::fs::File::open(s.path())? }))
    }

    fn local_path(&self) -> Option<&Path> {
        self.staged.as_ref().map(|s| s.path())
    }

    fn commit(mut self: Box<Self>) -> VfsResult<CommitReport> {
        let staged = self.staged.take().expect("double commit");
        let mut report = CommitReport::default();

        if let Some(ms) = self.hint.mtime_ms {
            let ft = filetime::FileTime::from_unix_time(ms.div_euclid(1000), (ms.rem_euclid(1000) * 1_000_000) as u32);
            if let Err(e) = filetime::set_file_mtime(staged.path(), ft) {
                report.mtime_error = Some(e.into());
            }
        }
        #[cfg(unix)]
        if let Some(mode) = self.hint.mode {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(mode)) {
                report.mode_error = Some(e.into());
            }
        }

        staged.commit()?;

        if self.hint.mtime_ms.is_some() {
            report.mtime_ondisk_ms = std::fs::metadata(&self.dst)
                .ok()
                .map(|md| crate::foundation::time::meta_mtime_ms(&md));
        }
        Ok(report)
    }
}

impl Vfs for LocalVfs {
    fn caps(&self) -> VfsCaps {
        VfsCaps {
            protocol: "local",
            // NTFS/APFS resolution is far under a millisecond; FAT's 2 s coarseness is
            // covered by the compare window's own 2 s floor, same as before the VFS.
            mtime_precision_ms: 1,
            set_mtime: Support::Yes,
            fsync: Support::Yes,
            rename: Support::Yes,
            rename_overwrite: Support::Yes,
            ranged_read: Support::Yes,
            write_at: Support::Yes,
            unix_mode: if cfg!(unix) { Support::Yes } else { Support::No },
            symlink: if cfg!(unix) { Support::Yes } else { Support::Unknown },
            file_id: if cfg!(unix) { Support::Yes } else { Support::No },
            free_space: Support::Yes,
            read_back: Support::Yes,
            local_trash: true,
            case_sensitivity: CaseSense::Unknown,
            max_parallel_streams: 16,
        }
    }

    fn display(&self) -> String {
        self.root_str.clone()
    }

    fn identity(&self) -> String {
        self.root_str.clone()
    }

    fn as_local(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn connect(&self) -> VfsResult<()> {
        Ok(())
    }

    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        match std::fs::symlink_metadata(self.abs(rel)) {
            Ok(md) => Ok(Some(meta_of(&md))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.abs(rel))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // lstat semantics, and a file vanishing between listing and stat is a scan race, not an error
            let md = match std::fs::symlink_metadata(entry.path()) {
                Ok(md) => md,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            out.push(VDirEntry { name, meta: meta_of(&md) });
        }
        Ok(out)
    }

    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        Ok(Box::new(LocalRead { file: std::fs::File::open(self.abs(rel))? }))
    }

    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        let mut f = std::fs::File::open(self.abs(rel))?;
        f.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; len as usize];
        let mut got = 0usize;
        while got < buf.len() {
            let n = f.read(&mut buf[got..])?;
            if n == 0 {
                break; // short only at EOF, per contract
            }
            got += n;
        }
        buf.truncate(got);
        Ok(buf)
    }

    fn read_link(&self, rel: &str) -> VfsResult<String> {
        Ok(std::fs::read_link(self.abs(rel))?.to_string_lossy().into_owned())
    }

    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        Ok(std::fs::create_dir_all(self.abs(rel))?)
    }

    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        let dst = self.abs(rel);
        let staged = Staged::create(&dst)?;
        Ok(Box::new(LocalStaged { staged: Some(staged), dst, hint: hint.clone() }))
    }

    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        Ok(std::fs::rename(self.abs(from_rel), self.abs(to_rel))?)
    }

    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        Ok(std::fs::remove_file(self.abs(rel))?)
    }

    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        Ok(std::fs::remove_dir(self.abs(rel))?)
    }

    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        let ft = filetime::FileTime::from_unix_time(
            mtime_ms.div_euclid(1000),
            (mtime_ms.rem_euclid(1000) * 1_000_000) as u32,
        );
        Ok(filetime::set_file_mtime(self.abs(rel), ft)?)
    }

    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            Ok(std::fs::set_permissions(self.abs(rel), std::fs::Permissions::from_mode(mode))?)
        }
        #[cfg(not(unix))]
        {
            let _ = (rel, mode);
            Err(super::VfsError::new(super::VfsErrorKind::Unsupported, "unix modes do not exist on this filesystem"))
        }
    }

    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        #[cfg(unix)]
        {
            Ok(std::os::unix::fs::symlink(target, self.abs(rel))?)
        }
        #[cfg(windows)]
        {
            // Needs developer mode or a privilege; when it fails, the error says so
            // and preflight's Unknown-capability listing already warned.
            Ok(std::os::windows::fs::symlink_file(target, self.abs(rel))?)
        }
    }

    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        Ok(disk_space(&self.root))
    }
}

/// (available, total) in bytes. Same implementation as the guard's path-based probe —
/// duplicated here rather than referenced because `fs` sits below `pipeline` in the
/// layer graph and must not reach up.
#[cfg(windows)]
fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lp_directory_name: *const u16,
            lp_free_bytes_available_to_caller: *mut u64,
            lp_total_number_of_bytes: *mut u64,
            lp_total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 {
        None
    } else {
        Some((avail, total))
    }
}

#[cfg(unix)]
fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        let unit = if st.f_frsize > 0 { st.f_frsize as u64 } else { st.f_bsize as u64 };
        Some((st.f_bavail as u64 * unit, st.f_blocks as u64 * unit))
    }
}
