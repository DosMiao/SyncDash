//! Descriptor-relative in-place patching for large local updates.

use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::{LocalRoot, LocalStagedFile};

use super::DELTA_MEM_CAP;

pub(super) struct StagedDeltaUpdate {
    pub(super) written_bytes: u64,
    pub(super) output_size: u64,
    pub(super) output_hash: String,
}

pub(super) fn stage_delta_update(
    source_root: &LocalRoot,
    target_root: &LocalRoot,
    relative: &RootRelativePath,
    staged: &mut LocalStagedFile,
    mut checkpoint: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<Option<StagedDeltaUpdate>> {
    let source_metadata = match source_root.metadata_path(relative) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let target_metadata = match target_root.metadata_path(relative) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if source_metadata.len() < crate::model::chunk::DELTA_MIN_SIZE
        || source_metadata.len() > DELTA_MEM_CAP
        || target_metadata.len() > DELTA_MEM_CAP
    {
        return Ok(None);
    }

    let existing_bytes = target_root.read(relative)?;
    let replacement_bytes = source_root.read(relative)?;
    checkpoint()?;
    staged.write_at(0, &existing_bytes)?;
    let existing_chunks = crate::fs::chunk::chunk_bytes(&existing_bytes);
    let replacement_chunks = crate::fs::chunk::chunk_bytes(&replacement_bytes);
    let existing_chunks_by_hash: std::collections::HashMap<&str, (u64, u32)> = existing_chunks
        .iter()
        .map(|chunk| (chunk.hash.as_str(), (chunk.off, chunk.len)))
        .collect();
    let mut written_bytes = 0u64;
    for chunk in &replacement_chunks {
        checkpoint()?;
        let start = chunk.off as usize;
        let end = start + chunk.len as usize;
        if existing_chunks_by_hash.get(chunk.hash.as_str()) == Some(&(chunk.off, chunk.len)) {
            continue;
        }
        staged.write_at(chunk.off, &replacement_bytes[start..end])?;
        written_bytes += chunk.len as u64;
    }
    if replacement_bytes.len() < existing_bytes.len() {
        checkpoint()?;
        staged.set_len(replacement_bytes.len() as u64)?;
    }

    Ok(Some(StagedDeltaUpdate {
        written_bytes,
        output_size: replacement_bytes.len() as u64,
        output_hash: blake3::hash(&replacement_bytes).to_hex().to_string(),
    }))
}
