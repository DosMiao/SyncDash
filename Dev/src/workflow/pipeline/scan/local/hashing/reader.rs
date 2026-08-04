//! Stable content reads and post-read identity verification for local files.

use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;
use crate::model::digest::Blake3Digest;

/// Read granularity, and therefore how often cancel and pause are honored mid-file.
const READ_CHUNK: u64 = 8 * 1024 * 1024;

/// The file a content read is about, together with the identity that read is only valid for.
///
/// The five values travel as one because the post-read verification is what makes the digest
/// trustworthy: a read that returns a hash without proving the bytes still belong to the size,
/// mtime, and inode the walk recorded has produced evidence for a file that no longer exists.
/// Threading them separately let a caller supply four of the five.
pub(in crate::pipeline::scan::local) struct LocalFileRead<'a> {
    pub root: &'a LocalRoot,
    pub relative: &'a RootRelativePath,
    pub size: u64,
    pub raw_mtime_ms: i64,
    pub expected_file_id: Option<&'a str>,
}

pub(in crate::pipeline::scan::local) fn full_hash_with_buffer<C, P>(
    target: &LocalFileRead<'_>,
    buffer: &mut Vec<u8>,
    mut checkpoint: C,
    mut on_read: P,
) -> std::io::Result<Blake3Digest>
where
    C: FnMut() -> std::io::Result<()>,
    P: FnMut(u64),
{
    use std::io::Read;

    let size = target.size;
    let mut file = target.root.open_read(target.relative)?;
    let width = size.clamp(1, READ_CHUNK) as usize;
    if buffer.len() < width {
        buffer.resize(width, 0);
    }
    let mut hasher = blake3::Hasher::new();
    let mut remaining = size;
    while remaining > 0 {
        checkpoint()?;
        let read_width = remaining.min(width as u64) as usize;
        let count = file.read(&mut buffer[..read_width])?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "file shrank while hashing: expected {size} bytes, read {}",
                    size - remaining
                ),
            ));
        }
        hasher.update(&buffer[..count]);
        on_read(count as u64);
        remaining -= count as u64;
    }
    verify_opened_file(&file, target)?;
    verify_current_file(target)?;
    Ok(Blake3Digest::from_hash(hasher.finalize()))
}

pub(super) fn sampled_hash_with_buffer<P>(
    target: &LocalFileRead<'_>,
    buffer: &mut Vec<u8>,
    on_read: P,
) -> std::io::Result<Blake3Digest>
where
    P: FnMut(u64),
{
    let mut file = target.root.open_read(target.relative)?;
    let digest = crate::pipeline::scan::digest::sampled_digest_stream(
        &mut file,
        target.size,
        buffer,
        on_read,
    )?;
    verify_opened_file(&file, target)?;
    verify_current_file(target)?;
    Ok(digest)
}

/// The handle the bytes were read through still describes the recorded identity.
fn verify_opened_file(file: &std::fs::File, target: &LocalFileRead<'_>) -> std::io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() != target.size
        || standard_mtime_ms(&metadata) != target.raw_mtime_ms
        || target.expected_file_id.is_some_and(|expected| {
            crate::fs::meta::standard_file_id(&metadata).as_deref() != Some(expected)
        })
    {
        return Err(std::io::Error::other(
            "file changed while content evidence was being read",
        ));
    }
    Ok(())
}

/// …and so does the name, re-stated through the capability root. Both checks are needed: the
/// handle cannot see a replacement published under the same path, and the path cannot see a
/// truncation of the object that was actually read.
fn verify_current_file(target: &LocalFileRead<'_>) -> std::io::Result<()> {
    let metadata = target.root.metadata_path(target.relative)?;
    if !metadata.is_file()
        || metadata.len() != target.size
        || capability_mtime_ms(&metadata) != target.raw_mtime_ms
        || target.expected_file_id.is_some_and(|expected| {
            crate::fs::meta::capability_file_id(&metadata).as_deref() != Some(expected)
        })
    {
        return Err(std::io::Error::other(
            "file changed while content evidence was being read",
        ));
    }
    Ok(())
}

pub(in crate::pipeline::scan::local) fn standard_mtime_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn capability_mtime_ms(metadata: &cap_primitives::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.into_std().duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
