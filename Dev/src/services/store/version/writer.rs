//! Preserving originals before they are overwritten or deleted.
//!
//! The writer holds one authority the rest of the module must not duplicate: whether a payload has
//! been written but not yet indexed. An unindexed payload is content that exists on disk with
//! nothing pointing at it, and the finish path is the only place allowed to decide what happens to
//! it — which is why the flag lives on the writer and is passed into the branches rather than
//! recomputed by them.
//!
//! Every write re-checks the lease. A preservation that lands after the lease is gone is writing
//! into a root another process now owns.

use std::io::Write;

use super::content::{
    capability_mtime_ms, file_mtime_ms, hash_local_file, invalid_data, parent_directory,
    read_version_metadata, relative_directory, relative_path, validate_version_metadata_size,
    REVERSE_DELTA_MAX_SIZE,
};
use super::model::{PreservedEntry, VersionIndexEntry, VersionManifest, VersionPayloadKind};
use super::retention::list_from_local_root;
use crate::foundation::path::{to_native, EntryName, RootRelativeDir, RootRelativePath};
use crate::fs::local_root::LocalRoot;
use crate::model::chunk::RecipeStep;

/// Builds one root-confined version while the caller supplies its active lease checkpoints.
pub struct VersionWriter {
    pub(super) root: LocalRoot,
    pub(super) version_dir: RootRelativeDir,
    pub(super) id: EntryName,
    pub(super) entries: Vec<PreservedEntry>,
    pub(super) bytes: u64,
    pub(super) has_unindexed_payload: bool,
}

impl VersionWriter {
    pub fn begin(
        root: &LocalRoot,
        mut checkpoint: impl FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<VersionWriter> {
        let token = crate::fs::vfs::random_name_token().map_err(std::io::Error::from)?;
        let id = EntryName::try_from(format!("{}-{}", crate::foundation::time::now_ms(), token))
            .map_err(|error| invalid_data(error.to_string()))?;
        let store = relative_directory(crate::foundation::names::VERSION_STORE_DIR)?;
        checkpoint()?;
        root.create_directory_all(&store)?;
        checkpoint()?;
        root.open_directory(&store)?.create_child_directory(&id)?;
        let version_dir = relative_directory(format!(
            "{}/{}",
            crate::foundation::names::VERSION_STORE_DIR,
            id.as_str()
        ))?;
        Ok(VersionWriter {
            root: root.clone(),
            version_dir,
            id,
            entries: Vec::new(),
            bytes: 0,
            has_unindexed_payload: false,
        })
    }

    /// Retain the current entry before its plan operation deletes or replaces it.
    /// `new_content` enables a bounded reverse delta for a regular-file replacement.
    pub fn preserve<F>(
        &mut self,
        rel: &RootRelativePath,
        new_content: Option<(&LocalRoot, &RootRelativePath)>,
        reason: &str,
        fsync: bool,
        mut checkpoint: F,
    ) -> std::io::Result<()>
    where
        F: FnMut() -> std::io::Result<()>,
    {
        if self.has_unindexed_payload {
            return Err(std::io::Error::other(format!(
                "version {} already contains a displaced payload that could not be indexed; further preservation is blocked",
                self.root
                    .display_path()
                    .join(to_native(self.version_dir.as_str()))
                    .display()
            )));
        }

        let initial_metadata = self.root.metadata_path(rel)?;
        let is_link = initial_metadata.file_type().is_symlink();
        let mut opened_original = if is_link {
            None
        } else {
            Some(self.root.open_read(rel)?)
        };
        let regular_metadata = opened_original
            .as_ref()
            .map(std::fs::File::metadata)
            .transpose()?;
        let old_size = regular_metadata
            .as_ref()
            .map_or(initial_metadata.len(), std::fs::Metadata::len);
        let new_metadata = new_content
            .map(|(root, path)| root.metadata_path(path))
            .transpose()?;
        let use_reverse_delta = !is_link
            && (crate::model::chunk::DELTA_MIN_SIZE..=REVERSE_DELTA_MAX_SIZE).contains(&old_size)
            && new_metadata.as_ref().is_some_and(|metadata| {
                metadata.is_file() && metadata.len() <= REVERSE_DELTA_MAX_SIZE
            });

        if use_reverse_delta {
            let (new_root, new_relative) =
                new_content.expect("reverse-delta eligibility requires replacement content");
            let new_chunks = crate::fs::chunk::chunk_file(new_root, new_relative)?;
            if new_metadata
                .as_ref()
                .is_some_and(|metadata| metadata.len() != new_chunks.size)
            {
                return Err(invalid_data(format!(
                    "replacement changed size while preparing reverse-delta preservation for {:?}",
                    rel.as_str()
                )));
            }
            let mut replacement_chunk_locations: std::collections::HashMap<
                (String, u32),
                (u64, u32),
            > = std::collections::HashMap::new();
            for chunk in new_chunks.chunks {
                replacement_chunk_locations
                    .entry((chunk.hash, chunk.len))
                    .or_insert((chunk.off, chunk.len));
            }
            let mut recipe: Vec<RecipeStep> = Vec::new();
            let blob_path = relative_path(format!(
                "{}/rdelta/{}",
                self.version_dir.as_str(),
                rel.as_str()
            ))?;
            checkpoint()?;
            self.root
                .create_directory_all(&parent_directory(&blob_path))?;
            checkpoint()?;
            let mut staged = self.root.create_staged(&blob_path)?;
            let mut blob_size = 0u64;
            let old_summary = crate::fs::chunk::visit_chunks(
                opened_original
                    .as_mut()
                    .expect("regular entries retain an opened original"),
                |chunk, bytes| {
                    if let Some(&(offset, length)) =
                        replacement_chunk_locations.get(&(chunk.hash.clone(), chunk.len))
                    {
                        recipe.push(RecipeStep {
                            s: "base".into(),
                            off: offset,
                            len: length,
                        });
                    } else {
                        let offset = blob_size;
                        staged.write_all(bytes)?;
                        blob_size = blob_size.checked_add(bytes.len() as u64).ok_or_else(|| {
                            invalid_data(format!(
                                "reverse-delta blob size overflows for {:?}",
                                rel.as_str()
                            ))
                        })?;
                        recipe.push(RecipeStep {
                            s: "blob".into(),
                            off: offset,
                            len: chunk.len,
                        });
                    }
                    Ok(())
                },
            )?;
            if old_summary.size != old_size {
                return Err(invalid_data(format!(
                    "original changed size while preparing reverse-delta preservation for {:?}",
                    rel.as_str()
                )));
            }
            let updated_bytes = self.bytes.checked_add(blob_size).ok_or_else(|| {
                invalid_data(
                    "version payload byte count overflowed while preserving a reverse delta",
                )
            })?;
            staged.seal(fsync)?;
            staged.commit_noreplace()?;
            self.has_unindexed_payload = true;
            checkpoint()?;
            self.root.remove_open_file(
                rel,
                opened_original
                    .as_ref()
                    .expect("regular entries retain an opened original"),
            )?;
            let regular_metadata = regular_metadata.expect("regular metadata was collected above");
            let old_mtime_ms = file_mtime_ms(&regular_metadata)?;
            let old_mode = crate::fs::meta::standard_unix_mode(&regular_metadata);
            self.bytes = updated_bytes;
            self.entries.push(PreservedEntry {
                relative_path: rel.clone(),
                payload_kind: VersionPayloadKind::ReverseDelta,
                reason: reason.into(),
                old_hash: old_summary.hash,
                old_size,
                old_mtime_ms,
                old_mode,
                new_hash: Some(new_chunks.hash),
                recipe: Some(recipe),
            });
            self.has_unindexed_payload = false;
            if fsync {
                self.root.sync_parent(rel)?;
            }
        } else {
            drop(opened_original.take());
            let preserved_path = relative_path(format!(
                "{}/files/{}",
                self.version_dir.as_str(),
                rel.as_str()
            ))?;
            checkpoint()?;
            self.root
                .create_directory_all(&parent_directory(&preserved_path))?;
            checkpoint()?;
            self.root.rename_noreplace(rel, &preserved_path)?;
            self.has_unindexed_payload = true;
            let preserved_metadata = self.root.metadata_path(&preserved_path)?;
            let old_size = preserved_metadata.len();
            let old_mtime_ms = capability_mtime_ms(&preserved_metadata)?;
            let old_mode = crate::fs::meta::capability_unix_mode(&preserved_metadata);
            let old_hash = if preserved_metadata.file_type().is_symlink() {
                String::new()
            } else if preserved_metadata.is_file() {
                hash_local_file(&self.root, &preserved_path)?
            } else {
                return Err(invalid_data(format!(
                    "preserved entry {:?} is neither a regular file nor a symlink",
                    rel.as_str()
                )));
            };
            let updated_bytes = self.bytes.checked_add(old_size).ok_or_else(|| {
                invalid_data("version payload byte count overflowed while preserving a whole entry")
            })?;
            self.bytes = updated_bytes;
            self.entries.push(PreservedEntry {
                relative_path: rel.clone(),
                payload_kind: VersionPayloadKind::Whole,
                reason: reason.into(),
                old_hash,
                old_size,
                old_mtime_ms,
                old_mode,
                new_hash: None,
                recipe: None,
            });
            self.has_unindexed_payload = false;
            // Once the original has moved, keep its manifest entry even if a
            // directory durability flush fails. Returning the flush error still fails the apply
            // item, while `finish` can index the recoverable payload instead of deleting it as an
            // apparently empty version.
            if fsync {
                self.root.sync_parent(&preserved_path)?;
                if crate::foundation::path::parent(rel.as_str())
                    != crate::foundation::path::parent(preserved_path.as_str())
                {
                    self.root.sync_parent(rel)?;
                }
            }
        }
        Ok(())
    }

    /// Publish payload metadata before adding the version to the root index.
    /// An unindexed displaced payload is retained for manual recovery and fails finalization.
    pub fn finish(
        self,
        ops: &[crate::model::plan::Op],
        fsync: bool,
        mut checkpoint: impl FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<Option<String>> {
        if self.has_unindexed_payload {
            return Err(std::io::Error::other(format!(
                "version payload remains recoverable but unindexed at {}",
                self.root
                    .display_path()
                    .join(to_native(self.version_dir.as_str()))
                    .display()
            )));
        }
        if self.entries.is_empty() {
            checkpoint()?;
            match self.root.remove_directory_all(&self.version_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            return Ok(None);
        }
        let mut plan_body = Vec::new();
        for op in ops {
            writeln!(plan_body, "{}", serde_json::to_string(op)?)?;
        }
        let manifest = VersionManifest {
            id: self.id.clone(),
            ts_ms: crate::foundation::time::now_ms(),
            host: crate::foundation::machine::host_name(),
            entries: self.entries.clone(),
        };
        let manifest_body = serde_json::to_vec_pretty(&manifest)?;
        validate_version_metadata_size(&manifest_body, "version manifest")?;
        let index_entry = VersionIndexEntry {
            id: self.id.clone(),
            ts_ms: manifest.ts_ms,
            host: manifest.host.clone(),
            ops: ops.len() as u64,
            preserved: self.entries.len() as u64,
            bytes: self.bytes,
        };
        let index_path = relative_path(format!(
            "{}/index.jsonl",
            crate::foundation::names::VERSION_STORE_DIR
        ))?;
        let mut index_body = match read_version_metadata(&self.root, &index_path, "version index") {
            Ok(bytes) => {
                list_from_local_root(&self.root)?;
                bytes
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        if !index_body.is_empty() && !index_body.ends_with(b"\n") {
            index_body.push(b'\n');
        }
        writeln!(index_body, "{}", serde_json::to_string(&index_entry)?)?;
        validate_version_metadata_size(&index_body, "version index")?;

        checkpoint()?;
        let plan_path = relative_path(format!("{}/plan.jsonl", self.version_dir.as_str()))?;
        let mut plan_write = self.root.create_staged(&plan_path)?;
        plan_write.write_all_from(&mut &plan_body[..])?;
        plan_write.seal(fsync)?;
        checkpoint()?;
        plan_write.commit_noreplace()?;

        checkpoint()?;
        let manifest_path = relative_path(format!("{}/manifest.json", self.version_dir.as_str()))?;
        let mut manifest_write = self.root.create_staged(&manifest_path)?;
        manifest_write.write_all_from(&mut &manifest_body[..])?;
        manifest_write.seal(fsync)?;
        checkpoint()?;
        manifest_write.commit_noreplace()?;

        checkpoint()?;
        let mut index_write = self.root.create_staged(&index_path)?;
        index_write.write_all_from(&mut &index_body[..])?;
        index_write.seal(fsync)?;
        checkpoint()?;
        index_write.commit()?;
        Ok(Some(self.id.into_string()))
    }
}
