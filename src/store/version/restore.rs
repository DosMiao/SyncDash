//! Root-confined version restoration.
//!
//! A restore validates every selected payload under one root lease before changing a destination.
//! Each entry then stages its replacement, retains the current occupant by no-replace rename, and
//! publishes onto the absent final name. Rollback is attempted only while the same lease is current.

mod manifest;
mod transaction;

use std::path::Path;

use crate::foundation::path::{to_native, EntryName};
use crate::fs::local_root::LocalRoot;
use crate::fs::vfs::Support;

use self::manifest::{load_restore_entries, RestorePayload};
use self::transaction::restore_entry;
use super::{acquire_version_root, check_version_lease, invalid_data};

/// Restore selected version entries while retaining every displaced destination inside the root.
/// Empty `files` means all entries; a dry run validates the manifest and every selected payload.
pub fn restore(
    root: &Path,
    version: &str,
    files: &[String],
    dry_run: bool,
) -> std::io::Result<(u64, u64, u64)> {
    let version = EntryName::try_from(version).map_err(|e| invalid_data(e.to_string()))?;
    if dry_run {
        let local_root = LocalRoot::open(root.to_path_buf())?;
        let entries = load_restore_entries(&local_root, &version, files)?;
        for validated in &entries {
            println!(
                "DRY  restore {} ({}, {} B, {})",
                validated.entry.relative_path,
                validated.entry.payload_kind,
                validated.entry.old_size,
                validated.entry.reason
            );
        }
        return Ok((0, entries.len() as u64, 0));
    }

    let locked = acquire_version_root(root)?;
    let entries = load_restore_entries(&locked.root, &version, files)?;
    if locked.caps.exclusive_entry_rename != Support::Yes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "restore requires atomic existing-entry no-replace rename",
        ));
    }
    if entries
        .iter()
        .any(|validated| matches!(validated.payload, RestorePayload::Symlink { .. }))
        && locked.caps.exclusive_symlink_publish != Support::Yes
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this root cannot publish restored symlinks exclusively",
        ));
    }

    let mut restored = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;
    let mut retention_session = None;
    let mut retained_destination_count = 0u64;
    for (index, validated) in entries.iter().enumerate() {
        match restore_entry(
            &locked.root,
            validated,
            &locked.lease,
            &mut retention_session,
            &mut retained_destination_count,
        ) {
            Ok(_) => {
                restored += 1;
                println!("OK   restored {}", validated.entry.relative_path);
            }
            Err(error) => {
                errors += 1;
                crate::log_error!(
                    "version",
                    "ERR  restore {}: {error}",
                    validated.entry.relative_path
                );
                if locked.lease.lease_lost() {
                    skipped += entries.len().saturating_sub(index + 1) as u64;
                    break;
                }
            }
        }
    }

    if retained_destination_count > 0 {
        if let Some(session) = &retention_session {
            println!(
                "displaced current files kept at: {}",
                locked
                    .root
                    .display_path()
                    .join(to_native(session.as_str()))
                    .display()
            );
        }
    } else if let Some(session) = &retention_session {
        if check_version_lease(&locked.lease).is_ok() {
            if let Err(error) = locked.root.remove_directory_all(session) {
                crate::log_warn!(
                    "version",
                    "could not remove empty restore-retention directory {}: {error}",
                    locked
                        .root
                        .display_path()
                        .join(to_native(session.as_str()))
                        .display()
                );
            }
        }
    }
    Ok((restored, skipped, errors))
}
