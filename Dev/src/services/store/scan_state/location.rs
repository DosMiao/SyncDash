//! Stable file placement for current and legacy scan-state indexes.

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

/// Pre-versioned state lowercased its complete logical identity.
pub(crate) fn legacy_logical_file(key: &str, extension: &str) -> PathBuf {
    file(key.to_lowercase().as_bytes(), extension)
}

/// Local state retains its historical path-derived filename; the header supplies physical binding.
pub(crate) fn local_file(key: &str, extension: &str) -> PathBuf {
    legacy_logical_file(key, extension)
}
