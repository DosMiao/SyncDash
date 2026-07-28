//! L1 on-disk state that outlives a single run.
//!
//! `settings` is app-level configuration, `trash` the local recycle store and its retention,
//! `version` the per-root version history, `hashcache` and `mtimefix` the two per-root tables the
//! scanner and the applier hand each other. All of them are best-effort: failing to record
//! something must never fail the sync itself.

pub mod hashcache;
pub mod migrate;
pub mod mtimefix;
pub mod settings;
pub mod trash;
pub mod version;

use std::path::PathBuf;

/// Where a per-root cache table lives, given the root's identity and a file extension.
///
/// The key is hashed rather than sanitized: a root phrase can hold a drive letter, a UNC prefix,
/// a URL with credentials, or CJK — none of which survives being turned into a filename, and all
/// of which must still map to one stable file. Lowercased first, so a Windows root spelled two
/// ways does not end up with two caches that each see half the tree as new.
pub(crate) fn cache_file(key: &str, ext: &str) -> PathBuf {
    let h = blake3::hash(key.to_lowercase().as_bytes());
    crate::foundation::dirs::data_dir().join("hashcache").join(format!("{}.{ext}", &h.to_hex()[..16]))
}
