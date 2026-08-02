//! Atomic writes (semantics modelled on syncthing `lib/osutil/atomic.go`, rewritten to match).
//!
//! Every write lands first in a temp file **in the same directory as the destination**; the data is fsynced, then renamed to the final name.
//! A same-volume rename is atomic: an interruption (power loss / network drop / Ctrl-C) leaves at most a temp file behind,
//! never half a file at the final path.
//!
//! This fixes a real data-loss path: `apply` used to call `fs::copy(src, dst)` directly, so an
//! Update interrupted mid-write → target left holding a truncated file with a fresh mtime →
//! the next sync compares it as "target-changed" → **the truncated file gets propagated back over source**.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::foundation::names::TEMP_PREFIX;

static NEXT_STAGE_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(unix)]
fn os_name_digest(name: &std::ffi::OsStr) -> blake3::Hash {
    use std::os::unix::ffi::OsStrExt;
    blake3::hash(name.as_bytes())
}

#[cfg(windows)]
fn os_name_digest(name: &std::ffi::OsStr) -> blake3::Hash {
    use std::os::windows::ffi::OsStrExt;
    let mut hasher = blake3::Hasher::new();
    for unit in name.encode_wide() {
        hasher.update(&unit.to_le_bytes());
    }
    hasher.finalize()
}

#[cfg(not(any(unix, windows)))]
fn os_name_digest(name: &std::ffi::OsStr) -> blake3::Hash {
    blake3::hash(name.to_string_lossy().as_bytes())
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Rename `source` to `destination` only if the destination is still absent.
/// A preceding existence check is not sufficient: another process can create the destination
/// between that check and a normal replacing rename.
#[cfg(target_os = "macos")]
pub(crate) fn atomic_rename_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path contains NUL")
    })?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "destination path contains NUL",
        )
    })?;
    if unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn atomic_rename_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path contains NUL")
    })?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "destination path contains NUL",
        )
    })?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(crate) fn atomic_rename_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    windows
)))]
pub(crate) fn atomic_rename_noreplace(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace rename is unavailable on this platform",
    ))
}

/// Persist directory-entry changes after an atomic local rename. Unix requires an explicit fsync
/// of the containing directory; syncing only the file does not make the new name crash-durable.
#[cfg(unix)]
pub(crate) fn sync_directory(directory: &Path) -> std::io::Result<()> {
    std::fs::File::open(directory)?.sync_all()
}

/// Both Windows rename primitives above use `MOVEFILE_WRITE_THROUGH`, which is the namespace
/// durability request available for these operations. There is no additional portable parent
/// directory flush to issue through `std`.
#[cfg(windows)]
pub(crate) fn sync_directory(_directory: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn sync_directory(_directory: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory fsync is unavailable on this platform",
    ))
}

/// Whether a file name (no directory part) is one of our temp files
pub fn is_temp_name(file_name: &str) -> bool {
    file_name.starts_with(TEMP_PREFIX)
}

/// Whether the last segment of a relative path ('/'-separated) is a temp file
pub fn is_temp_rel(rel: &str) -> bool {
    is_temp_name(crate::foundation::path::base_name(rel))
}

/// A staged write. Dropping it without a commit deletes the temp file automatically,
/// so any early `?` return leaves no debris behind.
pub struct Staged {
    tmp: PathBuf,
    dst: PathBuf,
    file: Option<std::fs::File>,
    committed: bool,
    sync_parent_on_commit: bool,
}

impl Write for Staged {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file already sealed"))?
            .write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file already sealed"))?
            .flush()
    }
}

impl Staged {
    /// Create the temp file in dst's own directory. Same directory = same volume, which is what makes commit's rename atomic
    /// (putting it in the system temp directory would degrade into a cross-volume copy and lose atomicity).
    pub fn create(dst: &Path) -> std::io::Result<Staged> {
        let dir = dst.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "destination has no parent directory",
            )
        })?;
        let basename = dst
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("out"));
        // Do not embed the whole basename: a perfectly valid NAME_MAX destination would make its
        // staging name too long. The digest is a bounded diagnostic hint; PID + monotonic ID and
        // create_new provide the actual collision exclusion.
        let basename_digest = os_name_digest(basename).to_hex();
        let basename_digest = &basename_digest.as_str()[..16];
        // PID separates processes; the monotonic stage ID separates simultaneous writers inside
        // one process. The old PID-only name let two concurrent cache rewrites unlink/truncate one
        // another's staging file and could publish the wrong generation.
        let (tmp, file) = loop {
            let stage_id = NEXT_STAGE_ID.fetch_add(1, Ordering::Relaxed);
            let tmp = dir.join(format!(
                "{TEMP_PREFIX}stage.{basename_digest}.{}.{stage_id}",
                std::process::id()
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
            {
                Ok(file) => break (tmp, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        Ok(Staged {
            tmp,
            dst: dst.to_path_buf(),
            file: Some(file),
            committed: false,
            sync_parent_on_commit: false,
        })
    }

    /// Path of the temp file (hash verification and mtime are applied to it; it becomes the final file only after commit)
    pub fn path(&self) -> &Path {
        &self.tmp
    }

    /// Write the reader's entire content into the staged file
    pub fn write_all_from(&mut self, r: &mut dyn std::io::Read) -> std::io::Result<u64> {
        let f = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file already sealed"))?;
        std::io::copy(r, f)
    }

    /// Write a run of data at a given offset (delta transfer: patch only the chunks that differ)
    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<()> {
        use std::io::Seek;
        let f = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file already sealed"))?;
        f.seek(std::io::SeekFrom::Start(offset))?;
        f.write_all(buf)
    }

    /// v0.9 M1: the one streaming copy loop — 1MiB buffer, a callback after every chunk
    /// (byte counting plus the cancel/pause checkpoint hang off it). An Err from on_chunk aborts:
    /// Drop clears the temp file and the final path is untouched.
    /// One loop serves all of: byte-level progress (M1), atomic writes (P0-1), the future write_at delta path (P1-1B).
    /// The callback receives **this chunk's bytes** (not just its length): post-copy verification validates against the
    /// full hash of the copy stream — the copy reads the whole file anyway, so hashing on the stream is free, and it decouples the check from scan-evidence depth (which may be only a sample).
    pub fn copy_from(
        &mut self,
        src: &Path,
        on_chunk: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
    ) -> std::io::Result<u64> {
        use std::io::Read;
        let f = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("staged file already sealed"))?;
        let mut reader = std::fs::File::open(src)?;
        let mut buf = vec![0u8; 1024 * 1024];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n])?;
            total += n as u64;
            on_chunk(&buf[..n])?;
        }
        Ok(total)
    }

    /// Flush the data to disk and close the handle.
    /// **Must be called before commit**: on Windows a rename fails while a write handle is open on the source or the destination.
    pub fn seal(&mut self, fsync: bool) -> std::io::Result<()> {
        self.sync_parent_on_commit |= fsync;
        if let Some(mut f) = self.file.take() {
            f.flush()?;
            if fsync {
                // fsync can be slow over SMB, but it is exactly what guarantees "after the rename the content is really on disk".
                // A job can turn it off with fsync=false when it needs maximum throughput (at its own risk).
                f.sync_all()?;
            }
        }
        Ok(())
    }

    /// Atomically replace the final file. seal must have been called first.
    pub fn commit(mut self) -> std::io::Result<()> {
        if self.file.is_some() {
            self.seal(true)?;
        }
        atomic_replace(&self.tmp, &self.dst)?;
        self.committed = true;
        if self.sync_parent_on_commit {
            sync_directory(self.dst.parent().expect("Staged::create checked parent"))?;
        }
        Ok(())
    }

    /// Atomically publish the staged file only while the final name remains absent.
    pub fn commit_noreplace(mut self) -> std::io::Result<()> {
        if self.file.is_some() {
            self.seal(true)?;
        }
        atomic_rename_noreplace(&self.tmp, &self.dst)?;
        self.committed = true;
        if self.sync_parent_on_commit {
            sync_directory(self.dst.parent().expect("Staged::create checked parent"))?;
        }
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            self.file.take(); // close the handle first, otherwise Windows will not let it be deleted
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("syncdash-atomic-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn commit_replaces_atomically() {
        let d = tmpdir("commit");
        let dst = d.join("f.txt");
        std::fs::write(&dst, b"old").unwrap();
        let mut s = Staged::create(&dst).unwrap();
        s.write_all_from(&mut &b"new content"[..]).unwrap();
        s.seal(true).unwrap();
        // before commit the final path still holds the old content — that is exactly what atomicity means
        assert_eq!(std::fs::read(&dst).unwrap(), b"old");
        s.commit().unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"new content");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn abandoned_stage_leaves_destination_untouched() {
        let d = tmpdir("abandon");
        let dst = d.join("f.txt");
        std::fs::write(&dst, b"original").unwrap();
        {
            let mut s = Staged::create(&dst).unwrap();
            s.write_all_from(&mut &b"half written..."[..]).unwrap();
            // drop without committing — simulating a mid-write failure
        }
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            b"original",
            "dst must never see partial content"
        );
        // the temp file must not survive either
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| is_temp_name(&e.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty(), "Drop must clean up the temp file");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn concurrent_stages_for_one_destination_never_share_a_temp_file() {
        let d = tmpdir("parallel-stage");
        let dst = d.join("state.jsonl");
        std::fs::write(&dst, b"old").unwrap();

        let mut first = Staged::create(&dst).unwrap();
        let mut second = Staged::create(&dst).unwrap();
        assert_ne!(first.path(), second.path());
        first.write_all(b"first").unwrap();
        second.write_all(b"second").unwrap();
        first.commit().unwrap();
        second.commit().unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), b"second");
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|entry| is_temp_name(&entry.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_replace_commit_preserves_a_destination_that_appeared_after_staging() {
        let d = tmpdir("no-replace");
        let dst = d.join("f.txt");
        let mut staged = Staged::create(&dst).unwrap();
        staged.write_all_from(&mut &b"planned move"[..]).unwrap();
        staged.seal(true).unwrap();

        std::fs::write(&dst, b"external writer").unwrap();
        let error = staged
            .commit_noreplace()
            .expect_err("an occupied name must be refused atomically");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&dst).unwrap(), b"external writer");
        assert!(
            std::fs::read_dir(&d)
                .unwrap()
                .flatten()
                .all(|entry| !is_temp_name(&entry.file_name().to_string_lossy())),
            "the refused stage must clean up its temp file",
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn concurrent_no_replace_publish_has_exactly_one_complete_winner() {
        let d = tmpdir("no-replace-race");
        let dst = d.join("winner.bin");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let payloads = [vec![0x31; 256 * 1024], vec![0xa7; 256 * 1024]];
        let mut threads = Vec::new();

        for payload in payloads.clone() {
            let dst = dst.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                let mut staged = Staged::create(&dst).unwrap();
                staged.write_all(&payload).unwrap();
                staged.seal(true).unwrap();
                barrier.wait();
                staged.commit_noreplace()
            }));
        }

        let outcomes: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        let loser = outcomes
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one publication must lose");
        assert_eq!(loser.kind(), std::io::ErrorKind::AlreadyExists);
        let landed = std::fs::read(&dst).unwrap();
        assert!(payloads.iter().any(|payload| payload == &landed));
        assert!(
            std::fs::read_dir(&d)
                .unwrap()
                .flatten()
                .all(|entry| !is_temp_name(&entry.file_name().to_string_lossy())),
            "the losing stage must remove its temporary file",
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn new_file_absent_until_commit() {
        let d = tmpdir("newfile");
        let dst = d.join("brand-new.bin");
        {
            let mut s = Staged::create(&dst).unwrap();
            s.write_all_from(&mut &b"xyz"[..]).unwrap();
            assert!(!dst.exists(), "destination must not appear before commit");
        }
        assert!(
            !dst.exists(),
            "abandoned write must not create the destination"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn near_name_max_destination_uses_a_bounded_staging_name() {
        let d = tmpdir("long-basename");
        let dst = d.join("x".repeat(240));
        let mut staged = Staged::create(&dst).expect("temp name must not repeat the long basename");
        let temp_name = staged.path().file_name().unwrap().to_string_lossy();
        assert!(temp_name.len() < 100, "unbounded temp name: {temp_name}");
        staged.write_all(b"long-name payload").unwrap();
        staged.seal(true).unwrap();
        staged.commit().unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"long-name payload");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_at_patches_offsets() {
        let d = tmpdir("patch");
        let dst = d.join("p.bin");
        let mut s = Staged::create(&dst).unwrap();
        s.write_all_from(&mut &b"AAAAAAAAAA"[..]).unwrap();
        s.write_at(3, b"ZZ").unwrap();
        s.seal(false).unwrap();
        s.commit().unwrap();
        let mut got = Vec::new();
        std::fs::File::open(&dst)
            .unwrap()
            .read_to_end(&mut got)
            .unwrap();
        assert_eq!(&got, b"AAAZZAAAAA");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_from_counts_and_aborts() {
        let d = tmpdir("copyfrom");
        let src = d.join("src.bin");
        std::fs::write(&src, vec![7u8; 3 * 1024 * 1024 + 123]).unwrap();
        let dst = d.join("dst.bin");
        // normal path: the bytes seen by the chunk callback sum to the file length
        {
            let mut s = Staged::create(&dst).unwrap();
            let mut seen = 0u64;
            let total = s
                .copy_from(&src, &mut |c| {
                    seen += c.len() as u64;
                    Ok(())
                })
                .unwrap();
            assert_eq!(total, 3 * 1024 * 1024 + 123);
            assert_eq!(seen, total);
            s.seal(false).unwrap();
            s.commit().unwrap();
        }
        assert_eq!(
            std::fs::metadata(&dst).unwrap().len(),
            3 * 1024 * 1024 + 123
        );
        // abort: call stop after the first chunk → dst unchanged, zero temp debris
        std::fs::write(&dst, b"keep me").unwrap();
        {
            let mut s = Staged::create(&dst).unwrap();
            let res = s.copy_from(&src, &mut |_| {
                Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "stop"))
            });
            assert!(res.is_err());
        }
        assert_eq!(std::fs::read(&dst).unwrap(), b"keep me");
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| is_temp_name(&e.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn temp_name_detection() {
        assert!(is_temp_rel("a/b/.syncdash.tmp.x.1234"));
        assert!(!is_temp_rel("a/b/normal.txt"));
        assert!(is_temp_name(".syncdash.tmp.foo"));
    }
}
