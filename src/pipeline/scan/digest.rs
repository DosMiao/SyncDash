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
    sampled_digest_with_buffer(path, size, &mut Vec::new(), |_| {})
}

pub(super) fn sampled_digest_with_buffer<P>(
    path: &Path,
    size: u64,
    buf: &mut Vec<u8>,
    mut on_read: P,
) -> std::io::Result<String>
where
    P: FnMut(u64),
{
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
            on_read(n as u64);
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("~{}", hasher.finalize().to_hex()))
}

pub(super) fn sampled_digest_vfs<P>(
    vfs: &dyn crate::fs::vfs::Vfs,
    rel: &str,
    size: u64,
    mut on_read: P,
) -> Result<String, crate::fs::vfs::error::VfsError>
where
    P: FnMut(u64),
{
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    for off in [0u64, size / 2, size.saturating_sub(SAMPLE_CHUNK as u64)] {
        let buf = vfs.read_range(rel, off, SAMPLE_CHUNK as u32)?;
        on_read(buf.len() as u64);
        hasher.update(&buf);
    }
    // Same windows, same size prefix, same `~` marker as the local sampled_digest —
    // the two lanes must produce identical digests for identical content
    Ok(format!("~{}", hasher.finalize().to_hex()))
}

fn full_hash_stream<R, C, P>(
    stream: &mut R,
    block: usize,
    expected_size: u64,
    mut checkpoint: C,
    mut on_read: P,
) -> Result<String, crate::fs::vfs::error::VfsError>
where
    R: std::io::Read + ?Sized,
    C: FnMut() -> Result<(), crate::fs::vfs::error::VfsError>,
    P: FnMut(u64),
{
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; block];
    let mut remaining = expected_size;
    while remaining > 0 {
        checkpoint()?;
        let width = remaining.min(buf.len() as u64) as usize;
        let n = std::io::Read::read(stream, &mut buf[..width])
            .map_err(crate::fs::vfs::error::VfsError::from)?;
        if n == 0 {
            return Err(crate::fs::vfs::error::VfsError::from(
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!(
                        "file shrank while hashing: expected {expected_size} bytes, read {}",
                        expected_size - remaining
                    ),
                ),
            ));
        }
        hasher.update(&buf[..n]);
        on_read(n as u64);
        remaining -= n as u64;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) fn full_hash_vfs<P>(
    vfs: &dyn crate::fs::vfs::Vfs,
    rel: &str,
    expected_size: u64,
    pp: &crate::obs::progress::PhaseProgress<'_>,
    on_read: P,
) -> Result<String, crate::fs::vfs::error::VfsError>
where
    P: FnMut(u64),
{
    let mut stream = vfs.open_read(rel)?;
    let block = stream.block_size().clamp(64 * 1024, 8 * 1024 * 1024);
    full_hash_stream(
        &mut *stream,
        block,
        expected_size,
        || {
            pp.checkpoint()
                .map_err(crate::fs::vfs::error::VfsError::from)
        },
        on_read,
    )
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
        let mut credited = 0u64;
        let first = sampled_digest_with_buffer(&f, data.len() as u64, &mut buf, |n| {
            credited += n;
        })
        .unwrap();
        assert_eq!(credited, effective_read(data.len() as u64, true));
        let capacity = buf.capacity();
        let second = sampled_digest_with_buffer(&f, data.len() as u64, &mut buf, |_| {}).unwrap();
        assert_eq!(first, second);
        assert_eq!(buf.capacity(), capacity);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn failed_stream_credits_only_successful_chunks() {
        struct OneChunkThenError(bool);

        impl std::io::Read for OneChunkThenError {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::other("injected read failure"));
                }
                self.0 = true;
                let n = 17.min(buffer.len());
                buffer[..n].fill(9);
                Ok(n)
            }
        }

        let mut stream = OneChunkThenError(false);
        let mut credited = Vec::new();
        let result = full_hash_stream(&mut stream, 64, 64, || Ok(()), |n| credited.push(n));

        assert!(result.is_err());
        assert_eq!(credited, vec![17]);
    }
}
