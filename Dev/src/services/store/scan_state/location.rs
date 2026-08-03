//! Stable file placement for scan-state indexes.

use std::path::PathBuf;

fn file(binding: &[u8], extension: &str) -> PathBuf {
    let hash = blake3::hash(binding);
    crate::foundation::dirs::data_dir()
        .join("hashcache")
        .join(format!("{}.{extension}", &hash.to_hex()[..16]))
}

/// Canonical VFS identities preserve case-sensitive user and root components.
pub(crate) fn logical_file(key: &str, extension: &str) -> PathBuf {
    file(key.as_bytes(), extension)
}

/// Local state keeps its historical name — blake3 over the lowercased root string — because a
/// changed formula silently invalidates every cache on every machine. The header, not the
/// filename, supplies physical binding.
pub(crate) fn local_file(key: &str, extension: &str) -> PathBuf {
    file(key.to_lowercase().as_bytes(), extension)
}
