//! Sampled content evidence: hash three windows and the size rather than the whole file.
//!
//! The two lanes each read bytes their own way, but the digest they produce must be identical or
//! a file would compare unequal to itself across backends — same windows, same size prefix, same
//! `~` marker. `local` reads with std::fs, the generic lane through `Vfs::read_range`.

use std::path::Path;

/// Parameters and implementation of the sampled digest (the fast rigor tier)
pub const SAMPLE_MIN: u64 = 4 * 1024 * 1024;
const SAMPLE_CHUNK: usize = 256 * 1024;
/// Bytes this scan will actually read in fast mode (only counting these makes the progress total and rate honest)
pub(super) fn effective_read(size: u64, sampled: bool) -> u64 {
    if sampled && size >= SAMPLE_MIN {
        (3 * SAMPLE_CHUNK as u64).min(size)
    } else {
        size
    }
}

#[cfg(test)]
pub(super) fn sampled_digest(path: &Path, size: u64) -> std::io::Result<String> {
    sampled_digest_with_buffer(path, size, &mut Vec::new())
}

pub(super) fn sampled_digest_with_buffer(
    path: &Path,
    size: u64,
    buf: &mut Vec<u8>,
) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    if buf.len() < SAMPLE_CHUNK {
        buf.resize(SAMPLE_CHUNK, 0);
    }
    for off in [0u64, size / 2, size.saturating_sub(SAMPLE_CHUNK as u64)] {
        f.seek(SeekFrom::Start(off))?;
        let mut read = 0usize;
        while read < SAMPLE_CHUNK {
            let n = f.read(&mut buf[read..SAMPLE_CHUNK])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("~{}", hasher.finalize().to_hex()))
}

pub(super) fn sampled_digest_vfs(
    vfs: &dyn crate::fs::vfs::Vfs,
    rel: &str,
    size: u64,
) -> Result<String, crate::fs::vfs::error::VfsError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    for off in [0u64, size / 2, size.saturating_sub(SAMPLE_CHUNK as u64)] {
        let buf = vfs.read_range(rel, off, SAMPLE_CHUNK as u32)?;
        hasher.update(&buf);
    }
    // Same windows, same size prefix, same `~` marker as the local sampled_digest —
    // the two lanes must produce identical digests for identical content
    Ok(format!("~{}", hasher.finalize().to_hex()))
}

pub(super) fn full_hash_vfs(
    vfs: &dyn crate::fs::vfs::Vfs,
    rel: &str,
    pp: &crate::obs::progress::PhaseProgress<'_>,
) -> Result<String, crate::fs::vfs::error::VfsError> {
    let mut stream = vfs.open_read(rel)?;
    let mut hasher = blake3::Hasher::new();
    let block = stream.block_size().clamp(64 * 1024, 8 * 1024 * 1024);
    let mut buf = vec![0u8; block];
    loop {
        pp.checkpoint()
            .map_err(crate::fs::vfs::error::VfsError::from)?; // cancel/pause between blocks
        let n = std::io::Read::read(&mut stream, &mut buf)
            .map_err(crate::fs::vfs::error::VfsError::from)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampled_digest_catches_edge_edits_only() {
        let d = std::env::temp_dir().join(format!("sd-sample-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("s.bin");
        let mut data = vec![5u8; 8 * 1024 * 1024];
        std::fs::write(&f, &data).unwrap();
        let a = sampled_digest(&f, data.len() as u64).unwrap();
        assert!(a.starts_with('~'), "sampled digests carry the ~ marker");
        // The midpoint falls inside a sample window → the digest must change
        data[4 * 1024 * 1024 + 10] = 9;
        std::fs::write(&f, &data).unwrap();
        let b = sampled_digest(&f, data.len() as u64).unwrap();
        assert_ne!(
            a, b,
            "an edit inside a sample window must change the digest"
        );
        // Offset 1MB lies outside all three sample windows → the digest does not change. That is fast mode's safety boundary; assert it honestly
        data[1024 * 1024] = 7;
        std::fs::write(&f, &data).unwrap();
        let c = sampled_digest(&f, data.len() as u64).unwrap();
        assert_eq!(
            b, c,
            "fast mode by design does not see edits outside sample windows"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sampled_digest_reuses_the_worker_buffer() {
        let d = std::env::temp_dir().join(format!("sd-sample-buffer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("sample.bin");
        let data = vec![13u8; SAMPLE_MIN as usize];
        std::fs::write(&f, &data).unwrap();

        let mut buf = Vec::new();
        let first = sampled_digest_with_buffer(&f, data.len() as u64, &mut buf).unwrap();
        let capacity = buf.capacity();
        let second = sampled_digest_with_buffer(&f, data.len() as u64, &mut buf).unwrap();
        assert_eq!(first, second);
        assert_eq!(buf.capacity(), capacity);

        let _ = std::fs::remove_dir_all(&d);
    }
}
