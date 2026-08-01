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
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
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
}

impl Write for Staged {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "staged file already sealed"))?
            .write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "staged file already sealed"))?
            .flush()
    }
}

impl Staged {
    /// Create the temp file in dst's own directory. Same directory = same volume, which is what makes commit's rename atomic
    /// (putting it in the system temp directory would degrade into a cross-volume copy and lose atomicity).
    pub fn create(dst: &Path) -> std::io::Result<Staged> {
        let dir = dst.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "destination has no parent directory")
        })?;
        let base = dst.file_name().and_then(|s| s.to_str()).unwrap_or("out");
        // PID separates processes; the monotonic stage ID separates simultaneous writers inside
        // one process. The old PID-only name let two concurrent cache rewrites unlink/truncate one
        // another's staging file and could publish the wrong generation.
        let (tmp, file) = loop {
            let stage_id = NEXT_STAGE_ID.fetch_add(1, Ordering::Relaxed);
            let tmp = dir.join(format!(
                "{TEMP_PREFIX}{base}.{}.{stage_id}",
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
        Ok(Staged { tmp, dst: dst.to_path_buf(), file: Some(file), committed: false })
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
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "staged file already sealed"))?;
        std::io::copy(r, f)
    }

    /// Write a run of data at a given offset (delta transfer: patch only the chunks that differ)
    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<()> {
        use std::io::Seek;
        let f = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "staged file already sealed"))?;
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
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "staged file already sealed"))?;
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
        assert_eq!(std::fs::read(&dst).unwrap(), b"original", "dst must never see partial content");
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
    fn new_file_absent_until_commit() {
        let d = tmpdir("newfile");
        let dst = d.join("brand-new.bin");
        {
            let mut s = Staged::create(&dst).unwrap();
            s.write_all_from(&mut &b"xyz"[..]).unwrap();
            assert!(!dst.exists(), "destination must not appear before commit");
        }
        assert!(!dst.exists(), "abandoned write must not create the destination");
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
        std::fs::File::open(&dst).unwrap().read_to_end(&mut got).unwrap();
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
            let total = s.copy_from(&src, &mut |c| { seen += c.len() as u64; Ok(()) }).unwrap();
            assert_eq!(total, 3 * 1024 * 1024 + 123);
            assert_eq!(seen, total);
            s.seal(false).unwrap();
            s.commit().unwrap();
        }
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), 3 * 1024 * 1024 + 123);
        // abort: call stop after the first chunk → dst unchanged, zero temp debris
        std::fs::write(&dst, b"keep me").unwrap();
        {
            let mut s = Staged::create(&dst).unwrap();
            let res = s.copy_from(&src, &mut |_| Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "stop")));
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
