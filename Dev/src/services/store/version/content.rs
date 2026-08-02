//! Reading version metadata and hashing local content, with the caps that bound both.
//!
//! Every limit here exists because the input is a file on disk that another process may have
//! written: a manifest is read with a size ceiling so a corrupt or hostile one is rejected before
//! it is parsed, not after it has been held in memory.

use std::io::Read;

use crate::foundation::path::{RootRelativeDir, RootRelativePath};
use crate::fs::local_root::LocalRoot;

pub(super) fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

pub(super) fn capability_mtime_ms(md: &cap_primitives::fs::Metadata) -> std::io::Result<i64> {
    Ok(crate::foundation::time::systime_ms(
        md.modified()?.into_std(),
    ))
}

pub(super) fn file_mtime_ms(md: &std::fs::Metadata) -> std::io::Result<i64> {
    Ok(crate::foundation::time::systime_ms(md.modified()?))
}

/// Read granularity for hashing an original before it is archived.
pub(super) const READ_CHUNK: u64 = 8 * 1024 * 1024;
pub(super) const REVERSE_DELTA_MAX_SIZE: u64 = 1024 * 1024 * 1024;
pub(super) const MAX_VERSION_METADATA_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn read_version_metadata(
    root: &LocalRoot,
    relative: &RootRelativePath,
    label: &str,
) -> std::io::Result<Vec<u8>> {
    let mut file = root.open_read(relative)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_VERSION_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_VERSION_METADATA_BYTES {
        return Err(invalid_data(format!(
            "{label} exceeds the {MAX_VERSION_METADATA_BYTES}-byte metadata limit"
        )));
    }
    Ok(bytes)
}

pub(super) fn validate_version_metadata_size(bytes: &[u8], label: &str) -> std::io::Result<()> {
    if bytes.len() as u64 > MAX_VERSION_METADATA_BYTES {
        Err(invalid_data(format!(
            "{label} exceeds the {MAX_VERSION_METADATA_BYTES}-byte metadata limit"
        )))
    } else {
        Ok(())
    }
}

pub(super) fn hash_local_file(
    root: &LocalRoot,
    relative: &RootRelativePath,
) -> std::io::Result<String> {
    let mut file = root.open_read(relative)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; file.metadata()?.len().clamp(1, READ_CHUNK) as usize];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) fn relative_path(value: impl Into<String>) -> std::io::Result<RootRelativePath> {
    RootRelativePath::new(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

pub(super) fn relative_directory(value: impl Into<String>) -> std::io::Result<RootRelativeDir> {
    RootRelativeDir::new(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

pub(super) fn parent_directory(path: &RootRelativePath) -> RootRelativeDir {
    RootRelativeDir::try_from(crate::foundation::path::parent(path.as_str()).unwrap_or(""))
        .expect("a validated relative path has a valid parent")
}
