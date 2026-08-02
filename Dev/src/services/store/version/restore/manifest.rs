use std::collections::HashSet;
use std::path::PathBuf;

use crate::foundation::path::{EntryName, RootRelativePath};
use crate::fs::local_root::LocalRoot;

use super::super::content::{hash_local_file, invalid_data, read_version_metadata, relative_path};
use super::super::model::{PreservedEntry, VersionManifest, VersionPayloadKind};

fn is_full_hash(value: &str) -> bool {
    crate::model::digest::is_blake3_hex(value)
}

fn require_full_hash(value: &str, label: &str, relative: &RootRelativePath) -> std::io::Result<()> {
    if is_full_hash(value) {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "{label} for version entry {:?} is not a full lowercase blake3 digest",
            relative.as_str()
        )))
    }
}

fn version_payload_path(
    version: &EntryName,
    payload_directory: &str,
    relative: &RootRelativePath,
) -> std::io::Result<RootRelativePath> {
    relative_path(format!(
        "{}/{}/{}/{}",
        crate::foundation::names::VERSION_STORE_DIR,
        version.as_str(),
        payload_directory,
        relative.as_str()
    ))
}

pub(super) enum RestorePayload {
    RegularFile { archive: RootRelativePath },
    Symlink { target: PathBuf },
    ReverseDelta { blob: RootRelativePath },
}

pub(super) struct ValidatedRestoreEntry {
    pub(super) entry: PreservedEntry,
    pub(super) payload: RestorePayload,
}

pub(super) fn verify_reverse_delta(
    root: &LocalRoot,
    entry: &PreservedEntry,
    relative: &RootRelativePath,
    blob_path: &RootRelativePath,
    mut checkpoint: impl FnMut() -> std::io::Result<()>,
    mut consume: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::{Read, Seek};

    let expected_base_hash = entry.new_hash.as_deref().ok_or_else(|| {
        invalid_data(format!(
            "reverse-delta version entry {:?} has no base hash",
            relative.as_str()
        ))
    })?;
    require_full_hash(expected_base_hash, "base hash", relative)?;
    let mut base = root.open_read(relative)?;
    let base_size = base.metadata()?.len();
    let mut buffer = vec![0u8; super::super::content::READ_CHUNK as usize];
    let mut base_hasher = blake3::Hasher::new();
    loop {
        checkpoint()?;
        let count = base.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        base_hasher.update(&buffer[..count]);
    }
    if base_hasher.finalize().to_hex().as_str() != expected_base_hash {
        return Err(invalid_data(format!(
            "current file no longer matches the reverse-delta base for {:?}",
            relative.as_str()
        )));
    }
    let mut blob = root.open_read(blob_path)?;
    let blob_size = blob.metadata()?.len();
    require_full_hash(&entry.old_hash, "archived content hash", relative)?;
    let recipe = entry.recipe.as_ref().ok_or_else(|| {
        invalid_data(format!(
            "reverse-delta version entry {:?} has no recipe",
            relative.as_str()
        ))
    })?;
    let mut reconstructed_size = 0u64;
    let mut reconstructed_hash = blake3::Hasher::new();
    let mut expected_blob_offset = 0u64;
    for step in recipe {
        if step.len == 0 {
            return Err(invalid_data(format!(
                "reverse-delta recipe for {:?} contains an empty step",
                relative.as_str()
            )));
        }
        let length = u64::from(step.len);
        let end = step.off.checked_add(length).ok_or_else(|| {
            invalid_data(format!(
                "reverse-delta range overflows for {:?}",
                relative.as_str()
            ))
        })?;
        let (source, source_size) = match step.s.as_str() {
            "base" => (&mut base, base_size),
            "blob" => {
                if step.off != expected_blob_offset {
                    return Err(invalid_data(format!(
                        "reverse-delta blob ranges are not contiguous for {:?}",
                        relative.as_str()
                    )));
                }
                expected_blob_offset = end;
                (&mut blob, blob_size)
            }
            other => {
                return Err(invalid_data(format!(
                    "reverse-delta recipe for {:?} has unknown source {other:?}",
                    relative.as_str()
                )))
            }
        };
        if end > source_size {
            return Err(invalid_data(format!(
                "reverse-delta recipe range is outside its {} payload for {:?}",
                step.s,
                relative.as_str()
            )));
        }
        let restored_end = reconstructed_size.checked_add(length).ok_or_else(|| {
            invalid_data(format!(
                "reconstructed size overflows for {:?}",
                relative.as_str()
            ))
        })?;
        if restored_end > entry.old_size {
            return Err(invalid_data(format!(
                "reverse-delta recipe exceeds the recorded size for {:?}",
                relative.as_str()
            )));
        }
        source.seek(std::io::SeekFrom::Start(step.off))?;
        let mut remaining = length;
        while remaining > 0 {
            checkpoint()?;
            let requested = usize::try_from(remaining.min(buffer.len() as u64))
                .expect("the request is bounded by the buffer length");
            let count = source.read(&mut buffer[..requested])?;
            if count == 0 {
                return Err(invalid_data(format!(
                    "reverse-delta {} payload ended inside a declared range for {:?}",
                    step.s,
                    relative.as_str()
                )));
            }
            reconstructed_hash.update(&buffer[..count]);
            consume(&buffer[..count])?;
            remaining -= count as u64;
        }
        reconstructed_size = restored_end;
    }
    if expected_blob_offset != blob_size {
        return Err(invalid_data(format!(
            "reverse-delta recipe references {expected_blob_offset} blob bytes for {:?}, but the payload contains {blob_size}",
            relative.as_str()
        )));
    }
    if reconstructed_size != entry.old_size {
        return Err(invalid_data(format!(
            "reverse-delta recipe reconstructed {} bytes for {:?}, expected {}",
            reconstructed_size,
            relative.as_str(),
            entry.old_size
        )));
    }
    if reconstructed_hash.finalize().to_hex().as_str() != entry.old_hash {
        return Err(invalid_data(format!(
            "reconstructed content hash does not match the version manifest for {:?}",
            relative.as_str()
        )));
    }
    Ok(())
}

fn validate_restore_payload(
    root: &LocalRoot,
    version: &EntryName,
    entry: PreservedEntry,
) -> std::io::Result<ValidatedRestoreEntry> {
    let relative = entry.relative_path.clone();
    let payload = match entry.payload_kind {
        VersionPayloadKind::Whole => {
            let archive = version_payload_path(version, "files", &relative)?;
            let metadata = root.metadata_path(&archive).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "cannot inspect whole-file payload for {:?}: {error}",
                        relative.as_str()
                    ),
                )
            })?;
            if metadata.file_type().is_symlink() {
                if !entry.old_hash.is_empty() {
                    return Err(invalid_data(format!(
                        "symlink payload for {:?} carries a regular-file hash",
                        relative.as_str()
                    )));
                }
                if metadata.len() != entry.old_size {
                    return Err(invalid_data(format!(
                        "symlink payload size for {:?} is {}, expected {}",
                        relative.as_str(),
                        metadata.len(),
                        entry.old_size
                    )));
                }
                RestorePayload::Symlink {
                    target: root.read_link(&archive)?,
                }
            } else if metadata.is_file() {
                require_full_hash(&entry.old_hash, "archived content hash", &relative)?;
                if metadata.len() != entry.old_size {
                    return Err(invalid_data(format!(
                        "whole-file payload size for {:?} is {}, expected {}",
                        relative.as_str(),
                        metadata.len(),
                        entry.old_size
                    )));
                }
                let actual_hash = hash_local_file(root, &archive)?;
                if actual_hash != entry.old_hash {
                    return Err(invalid_data(format!(
                        "whole-file payload hash does not match the version manifest for {:?}",
                        relative.as_str()
                    )));
                }
                RestorePayload::RegularFile { archive }
            } else {
                return Err(invalid_data(format!(
                    "whole-file payload for {:?} is neither a regular file nor a symlink",
                    relative.as_str()
                )));
            }
        }
        VersionPayloadKind::ReverseDelta => {
            let blob = version_payload_path(version, "rdelta", &relative)?;
            verify_reverse_delta(root, &entry, &relative, &blob, || Ok(()), |_| Ok(()))?;
            RestorePayload::ReverseDelta { blob }
        }
    };
    Ok(ValidatedRestoreEntry { entry, payload })
}

pub(super) fn load_restore_entries(
    root: &LocalRoot,
    version: &EntryName,
    files: &[String],
) -> std::io::Result<Vec<ValidatedRestoreEntry>> {
    let manifest_path = relative_path(format!(
        "{}/{}/manifest.json",
        crate::foundation::names::VERSION_STORE_DIR,
        version.as_str()
    ))?;
    let manifest: VersionManifest = serde_json::from_slice(&read_version_metadata(
        root,
        &manifest_path,
        "version manifest",
    )?)
    .map_err(|error| invalid_data(format!("invalid version manifest: {error}")))?;
    if manifest.id != *version {
        return Err(invalid_data(format!(
            "version manifest id {:?} does not match requested version {:?}",
            manifest.id.as_str(),
            version.as_str()
        )));
    }

    let mut selected_paths = HashSet::new();
    for file in files {
        let relative = RootRelativePath::try_from(file.as_str())
            .map_err(|error| invalid_data(error.to_string()))?;
        if crate::foundation::names::is_internal_artifact_path(&relative) {
            return Err(invalid_data(format!(
                "requested restore path {:?} is reserved for SyncDash metadata",
                relative.as_str()
            )));
        }
        if !selected_paths.insert(relative.clone()) {
            return Err(invalid_data(format!(
                "duplicate requested restore path {:?}",
                relative.as_str()
            )));
        }
    }

    let select_all = selected_paths.is_empty();
    let mut manifest_paths = HashSet::new();
    let mut selected_entries = Vec::new();
    for entry in manifest.entries {
        let relative = entry.relative_path.clone();
        if crate::foundation::names::is_internal_artifact_path(&relative) {
            return Err(invalid_data(format!(
                "version path {:?} is reserved for SyncDash metadata",
                relative.as_str()
            )));
        }
        if !manifest_paths.insert(relative.clone()) {
            return Err(invalid_data(format!(
                "duplicate path {:?} in version manifest",
                relative.as_str()
            )));
        }
        if entry.old_mode.is_some_and(|mode| mode > 0o7777) {
            return Err(invalid_data(format!(
                "invalid permission mode in version entry {:?}",
                relative.as_str()
            )));
        }
        match (
            entry.payload_kind,
            entry.new_hash.as_deref(),
            entry.recipe.as_deref(),
        ) {
            (VersionPayloadKind::Whole, None, None) => {
                if !entry.old_hash.is_empty() {
                    require_full_hash(&entry.old_hash, "archived content hash", &relative)?;
                }
            }
            (VersionPayloadKind::ReverseDelta, Some(base_hash), Some(recipe)) => {
                if entry.old_size < crate::model::chunk::DELTA_MIN_SIZE {
                    return Err(invalid_data(format!(
                        "reverse-delta version entry {:?} is below the reverse-delta size threshold",
                        relative.as_str()
                    )));
                }
                require_full_hash(&entry.old_hash, "archived content hash", &relative)?;
                require_full_hash(base_hash, "base hash", &relative)?;
                if recipe
                    .iter()
                    .any(|step| step.s != "base" && step.s != "blob")
                {
                    return Err(invalid_data(format!(
                        "invalid reverse-delta recipe source for {:?}",
                        relative.as_str()
                    )));
                }
            }
            (VersionPayloadKind::Whole, _, _) => {
                return Err(invalid_data(format!(
                    "whole-file version entry {:?} carries reverse-delta fields",
                    relative.as_str()
                )))
            }
            (VersionPayloadKind::ReverseDelta, _, _) => {
                return Err(invalid_data(format!(
                    "reverse-delta version entry {:?} is incomplete",
                    relative.as_str()
                )))
            }
        }
        if select_all || selected_paths.contains(&relative) {
            selected_entries.push(entry);
        }
    }

    if !select_all {
        let mut missing = selected_paths
            .difference(&manifest_paths)
            .map(|relative| relative.as_str())
            .collect::<Vec<_>>();
        missing.sort_unstable();
        if !missing.is_empty() {
            return Err(invalid_data(format!(
                "requested restore path(s) are absent from the version manifest: {}",
                missing.join(", ")
            )));
        }
    }

    selected_entries
        .into_iter()
        .map(|entry| validate_restore_payload(root, version, entry))
        .collect()
}
