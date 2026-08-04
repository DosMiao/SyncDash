//! Snapshot facts read off entry metadata, spelled once.
//!
//! The unix mode and file-id a scan records participate in compare evidence: two lanes spelling
//! the same fact differently would make an unchanged file read as changed, or an unmoved file as
//! renamed. Each fact therefore has one capture site per metadata flavor, and the flavors agree by
//! construction. Windows deliberately publishes neither: NTFS file indices can be reused after a
//! deletion, so eligibility is a volume capability (`vfs::local::volume::file_ids_stable_for_fs`)
//! rather than a host assumption, and Win32 has no unix permission bits to preserve.

/// The persisted `device:inode` spelling stored by snapshot tables and hashing evidence.
#[cfg(unix)]
fn spell_file_id(device: u64, inode: u64) -> String {
    format!("{device}:{inode}")
}

#[cfg(unix)]
pub fn file_id_std(metadata: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(spell_file_id(metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
pub fn file_id_std(_metadata: &std::fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
pub fn file_id_cap(metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    use cap_primitives::fs::MetadataExt;
    Some(spell_file_id(metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
pub fn file_id_cap(_metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    None
}

/// Permission bits worth preserving across a sync: the classic 12 (rwx plus setuid/setgid/sticky).
/// Higher `st_mode` bits carry the file type, which snapshots already record separately.
#[cfg(unix)]
const PRESERVED_MODE_BITS: u32 = 0o7777;

#[cfg(unix)]
pub fn unix_mode_std(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.mode() & PRESERVED_MODE_BITS)
}

#[cfg(not(unix))]
pub fn unix_mode_std(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
pub fn unix_mode_cap(metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    use cap_primitives::fs::MetadataExt;
    Some(metadata.mode() & PRESERVED_MODE_BITS)
}

#[cfg(not(unix))]
pub fn unix_mode_cap(_metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    None
}
