//! Stable content reads and post-read identity verification for local files.

use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;
use crate::model::digest::Blake3Digest;

/// Read granularity, and therefore how often cancel and pause are honored mid-file.
const READ_CHUNK: u64 = 8 * 1024 * 1024;

pub(in crate::pipeline::scan::local) fn full_hash_with_buffer<C, P>(
    root: &LocalRoot,
    relative: &RootRelativePath,
    size: u64,
    raw_mtime_ms: i64,
    expected_file_id: Option<&str>,
    buffer: &mut Vec<u8>,
    mut checkpoint: C,
    mut on_read: P,
) -> std::io::Result<Blake3Digest>
where
    C: FnMut() -> std::io::Result<()>,
    P: FnMut(u64),
{
    use std::io::Read;

    let mut file = root.open_read(relative)?;
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
    verify_opened_file(&file, size, raw_mtime_ms, expected_file_id)?;
    verify_current_file(root, relative, size, raw_mtime_ms, expected_file_id)?;
    Ok(Blake3Digest::from_hash(hasher.finalize()))
}

pub(super) fn sampled_hash_with_buffer<P>(
    root: &LocalRoot,
    relative: &RootRelativePath,
    size: u64,
    raw_mtime_ms: i64,
    expected_file_id: Option<&str>,
    buffer: &mut Vec<u8>,
    on_read: P,
) -> std::io::Result<Blake3Digest>
where
    P: FnMut(u64),
{
    let mut file = root.open_read(relative)?;
    let digest =
        crate::pipeline::scan::digest::sampled_digest_stream(&mut file, size, buffer, on_read);
    let digest = match digest {
        Ok(value) => value,
        Err(error) => return Err(error),
    };
    verify_opened_file(&file, size, raw_mtime_ms, expected_file_id)?;
    verify_current_file(root, relative, size, raw_mtime_ms, expected_file_id)?;
    Ok(digest)
}

fn verify_opened_file(
    file: &std::fs::File,
    size: u64,
    raw_mtime_ms: i64,
    expected_file_id: Option<&str>,
) -> std::io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() != size
        || standard_mtime_ms(&metadata) != raw_mtime_ms
        || expected_file_id
            .is_some_and(|expected| standard_file_id(&metadata).as_deref() != Some(expected))
    {
        return Err(std::io::Error::other(
            "file changed while content evidence was being read",
        ));
    }
    Ok(())
}

fn verify_current_file(
    root: &LocalRoot,
    relative: &RootRelativePath,
    size: u64,
    raw_mtime_ms: i64,
    expected_file_id: Option<&str>,
) -> std::io::Result<()> {
    let metadata = root.metadata_path(relative)?;
    if !metadata.is_file()
        || metadata.len() != size
        || capability_mtime_ms(&metadata) != raw_mtime_ms
        || expected_file_id
            .is_some_and(|expected| capability_file_id(&metadata).as_deref() != Some(expected))
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

#[cfg(unix)]
fn standard_file_id(metadata: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn standard_file_id(_metadata: &std::fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
fn capability_file_id(metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    use cap_primitives::fs::MetadataExt;
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn capability_file_id(_metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    None
}
