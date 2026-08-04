//! Snapshot facts read off entry metadata, spelled once.
//!
//! The unix mode and file-id a scan records participate in compare evidence: two lanes spelling
//! the same fact differently would make an unchanged file read as changed, or an unmoved file as
//! renamed. Each fact therefore has one capture site per metadata flavor, and the flavors agree by
//! construction. Windows deliberately publishes neither: NTFS file indices can be reused after a
//! deletion, so eligibility is a volume capability (`vfs::local::volume::file_ids_stable_for_fs`)
//! rather than a host assumption, and Win32 has no unix permission bits to preserve.
//!
//! `standard_` reads `std::fs::Metadata`, `capability_` reads the `cap_primitives` record a
//! confined root yields — the same prefixes the local scan lane uses for its mtime readers.

/// The persisted `device:inode` spelling stored by snapshot tables and hashing evidence.
#[cfg(unix)]
fn spell_file_id(device: u64, inode: u64) -> String {
    format!("{device}:{inode}")
}

#[cfg(unix)]
pub fn standard_file_id(metadata: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(spell_file_id(metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
pub fn standard_file_id(_metadata: &std::fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
pub fn capability_file_id(metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    use cap_primitives::fs::MetadataExt;
    Some(spell_file_id(metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
pub fn capability_file_id(_metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    None
}

/// Permission bits worth preserving across a sync: the classic 12 (rwx plus setuid/setgid/sticky).
/// Higher `st_mode` bits carry the file type, which snapshots already record separately.
#[cfg(unix)]
const PRESERVED_MODE_BITS: u32 = 0o7777;

#[cfg(unix)]
pub fn standard_unix_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.mode() & PRESERVED_MODE_BITS)
}

#[cfg(not(unix))]
pub fn standard_unix_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
pub fn capability_unix_mode(metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    use cap_primitives::fs::MetadataExt;
    Some(metadata.mode() & PRESERVED_MODE_BITS)
}

#[cfg(not(unix))]
pub fn capability_unix_mode(_metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    None
}
