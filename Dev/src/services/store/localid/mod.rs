//! Physical identity for machine-local scan state.
//!
//! `LocalVfs::identity()` intentionally remains the root exactly as the job spelled it because
//! changing that string would orphan every historical cache filename. Persistent state needs a
//! stronger answer inside that file, though: `/Volumes/Backup/Code` can name a different disk
//! after an unplug/replug. This helper binds a cache generation to both the mounted volume and the
//! canonical root within it, without changing the historical filename key.

mod macos;
mod unix;
mod windows;

#[cfg(test)]
mod tests;

// Exactly one arm defines `platform_identity` for any given target, so the router below carries
// the same predicates the definitions do. A target matching none of them fails to build here
// rather than silently binding scan state to an identity nobody verified.
#[cfg(target_os = "macos")]
use self::macos::platform_identity;
#[cfg(all(unix, not(target_os = "macos")))]
use self::unix::platform_identity;
#[cfg(windows)]
use self::windows::platform_identity;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalScanStateIdentity {
    cache_key: String,
    binding: Vec<u8>,
    file_ids_stable: bool,
    persistent_reuse: bool,
}

impl LocalScanStateIdentity {
    pub(crate) fn for_root(root: &Path) -> LocalScanStateIdentity {
        let canonical = canonical_or_absolute(root);
        let platform = platform_identity(&canonical);
        LocalScanStateIdentity {
            // The slot stays pinned to the pre-versioning formula. Headerless local JSONL remains
            // discoverable there, but is rejected and replaced after a successful bound scan.
            cache_key: root.to_string_lossy().into_owned(),
            binding: encode_binding(&platform.volume, &platform.relative_root),
            file_ids_stable: platform.file_ids_stable,
            persistent_reuse: platform.persistent_reuse,
        }
    }

    pub(crate) fn cache_key(&self) -> &str {
        &self.cache_key
    }

    pub(crate) fn binding(&self) -> &[u8] {
        &self.binding
    }

    /// FAT-family object identifiers are allocation artifacts, not durable rename evidence. They
    /// may be reused after deletion and can change across a remount, so callers must omit them
    /// from snapshots on those filesystems.
    pub(crate) fn file_ids_stable(&self) -> bool {
        self.file_ids_stable
    }

    /// Whether the platform supplied a volume identity that remains trustworthy across process
    /// restarts and media replacement. Callers must not read or write on-disk scan state otherwise.
    pub(crate) fn persistent_reuse(&self) -> bool {
        self.persistent_reuse
    }

    #[cfg(test)]
    pub(crate) fn injected(
        cache_key: impl Into<String>,
        binding: impl Into<Vec<u8>>,
        persistent_reuse: bool,
    ) -> LocalScanStateIdentity {
        LocalScanStateIdentity {
            cache_key: cache_key.into(),
            binding: binding.into(),
            file_ids_stable: false,
            persistent_reuse,
        }
    }
}

#[derive(Debug)]
struct PlatformIdentity {
    volume: Vec<u8>,
    relative_root: Vec<u8>,
    file_ids_stable: bool,
    persistent_reuse: bool,
}

fn encode_binding(volume: &[u8], relative_root: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + volume.len() + relative_root.len());
    out.extend_from_slice(b"syncdash.local-scan-state\0");
    out.extend_from_slice(&(volume.len() as u64).to_le_bytes());
    out.extend_from_slice(volume);
    out.extend_from_slice(&(relative_root.len() as u64).to_le_bytes());
    out.extend_from_slice(relative_root);
    out
}

fn canonical_or_absolute(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| {
        if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir().map_or_else(|_| root.to_path_buf(), |cwd| cwd.join(root))
        }
    })
}

#[cfg(test)]
fn fat_family(fs_name: &str) -> bool {
    matches!(
        fs_name.to_ascii_lowercase().as_str(),
        "fat" | "fat12" | "fat16" | "fat32" | "msdos" | "vfat" | "exfat"
    )
}

fn named_filesystem_has_stable_file_ids(fs_name: &str) -> bool {
    matches!(
        fs_name.to_ascii_lowercase().as_str(),
        "apfs"
            | "hfs"
            | "hfsplus"
            | "ext2"
            | "ext3"
            | "ext4"
            | "btrfs"
            | "xfs"
            | "zfs"
            | "f2fs"
            | "jfs"
            | "ntfs"
            | "ntfs3"
            | "refs"
    )
}
