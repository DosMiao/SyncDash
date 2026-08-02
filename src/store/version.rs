//! Optional in-root history for deleted and overwritten files.
//!
//! With `versioning = true`, preserved content lives in that root's `.version_syncDash/`, so the
//! history remains available from either machine that can access the root.
//!
//! Directory layout:
//!   <root>/.version_syncDash/
//!     index.jsonl                  one line per version {id, ts_ms, host, ops, preserved, bytes}
//!     <id>/plan.jsonl              the op list this run executed (audit trail)
//!     <id>/manifest.json           preserved entries: rel → whole|rdelta + the hashes + original mtime/mode
//!     <id>/files/<rel>             whole-file copy of the original (small files and deleted files)
//!     <id>/rdelta/<rel>            reverse-patch blob (overwritten files ≥4MB whose new content can serve as a reference)
//!
//! FastCDC reverse patches express the old file as chunks from the current file plus old-only blob
//! chunks. Restore verifies the current base hash and the reconstructed old hash.

use crate::model::chunk::RecipeStep;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

mod restore;

pub use restore::restore;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionPayloadKind {
    #[serde(rename = "whole")]
    Whole,
    #[serde(rename = "rdelta")]
    ReverseDelta,
}

impl std::fmt::Display for VersionPayloadKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Whole => "whole",
            Self::ReverseDelta => "rdelta",
        })
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PreservedEntry {
    #[serde(rename = "rel")]
    pub relative_path: RootRelativePath,
    #[serde(rename = "kind")]
    pub payload_kind: VersionPayloadKind,
    #[serde(rename = "why")]
    pub reason: String,
    pub old_hash: String,
    pub old_size: u64,
    pub old_mtime_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub old_mode: Option<u32>,
    /// rdelta: the hash the current file must match at reassembly time
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub new_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub recipe: Option<Vec<RecipeStep>>,
}

#[derive(Serialize, Deserialize)]
pub struct VersionManifest {
    pub id: EntryName,
    pub ts_ms: u64,
    pub host: String,
    pub entries: Vec<PreservedEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct VersionIndexEntry {
    pub id: EntryName,
    pub ts_ms: u64,
    pub host: String,
    pub ops: u64,
    pub preserved: u64,
    pub bytes: u64,
}

use crate::foundation::path::{to_native, EntryName, RootRelativeDir, RootRelativePath};
use crate::fs::local_root::LocalRoot;
use crate::fs::lock::RootLock;
use crate::fs::vfs::local::LocalVfs;
use crate::fs::vfs::{Support, Vfs, VfsCaps};

fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn capability_mtime_ms(md: &cap_primitives::fs::Metadata) -> std::io::Result<i64> {
    Ok(crate::foundation::time::systime_ms(
        md.modified()?.into_std(),
    ))
}

fn file_mtime_ms(md: &std::fs::Metadata) -> std::io::Result<i64> {
    Ok(crate::foundation::time::systime_ms(md.modified()?))
}

/// Read granularity for hashing an original before it is archived.
const READ_CHUNK: u64 = 8 * 1024 * 1024;
const REVERSE_DELTA_MAX_SIZE: u64 = 1024 * 1024 * 1024;
const MAX_VERSION_METADATA_BYTES: u64 = 64 * 1024 * 1024;

fn read_version_metadata(
    root: &LocalRoot,
    relative: &RootRelativePath,
    label: &str,
) -> std::io::Result<Vec<u8>> {
    let mut file = root.open_read(relative)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_VERSION_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_VERSION_METADATA_BYTES {
        return Err(invalid_data(format!(
            "{label} exceeds the {MAX_VERSION_METADATA_BYTES}-byte metadata limit"
        )));
    }
    Ok(bytes)
}

fn validate_version_metadata_size(bytes: &[u8], label: &str) -> std::io::Result<()> {
    if bytes.len() as u64 > MAX_VERSION_METADATA_BYTES {
        Err(invalid_data(format!(
            "{label} exceeds the {MAX_VERSION_METADATA_BYTES}-byte metadata limit"
        )))
    } else {
        Ok(())
    }
}

fn hash_local_file(root: &LocalRoot, relative: &RootRelativePath) -> std::io::Result<String> {
    let mut file = root.open_read(relative)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; file.metadata()?.len().clamp(1, READ_CHUNK) as usize];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn relative_path(value: impl Into<String>) -> std::io::Result<RootRelativePath> {
    RootRelativePath::new(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

fn relative_directory(value: impl Into<String>) -> std::io::Result<RootRelativeDir> {
    RootRelativeDir::new(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

fn parent_directory(path: &RootRelativePath) -> RootRelativeDir {
    RootRelativeDir::try_from(crate::foundation::path::parent(path.as_str()).unwrap_or(""))
        .expect("a validated relative path has a valid parent")
}

/// Builds one root-confined version while the caller supplies its active lease checkpoints.
pub struct VersionWriter {
    root: LocalRoot,
    version_dir: RootRelativeDir,
    id: EntryName,
    entries: Vec<PreservedEntry>,
    bytes: u64,
    has_unindexed_payload: bool,
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
            #[cfg(unix)]
            let old_mode = {
                use std::os::unix::fs::MetadataExt;
                Some(regular_metadata.mode() & 0o7777)
            };
            #[cfg(not(unix))]
            let old_mode: Option<u32> = None;
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
            #[cfg(unix)]
            let old_mode = {
                use cap_primitives::fs::MetadataExt;
                Some(preserved_metadata.mode() & 0o7777)
            };
            #[cfg(not(unix))]
            let old_mode: Option<u32> = None;
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
            host: crate::model::table::host_name(),
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

pub fn list(root: &Path) -> std::io::Result<Vec<VersionIndexEntry>> {
    let local_root = LocalRoot::open(root.to_path_buf())?;
    list_from_local_root(&local_root)
}

fn list_from_local_root(root: &LocalRoot) -> std::io::Result<Vec<VersionIndexEntry>> {
    let index_relative = relative_path(format!(
        "{}/index.jsonl",
        crate::foundation::names::VERSION_STORE_DIR
    ))?;
    let index_display = root.display_path().join(to_native(index_relative.as_str()));
    let bytes = match read_version_metadata(root, &index_relative, "version index") {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let text = String::from_utf8(bytes)
        .map_err(|error| invalid_data(format!("version index is not UTF-8: {error}")))?;
    let mut entries = Vec::new();
    let mut version_ids = HashSet::new();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: VersionIndexEntry = serde_json::from_str(line).map_err(|error| {
            invalid_data(format!(
                "invalid version index line {} in {}: {error}",
                line_index + 1,
                index_display.display()
            ))
        })?;
        if !version_ids.insert(entry.id.clone()) {
            return Err(invalid_data(format!(
                "duplicate version id {:?} on index line {}",
                entry.id,
                line_index + 1
            )));
        }
        entries.push(entry);
    }
    Ok(entries)
}

struct LockedVersionRoot {
    root: LocalRoot,
    lease: RootLock,
    caps: VfsCaps,
}

fn acquire_version_root(path: &Path) -> std::io::Result<LockedVersionRoot> {
    let local_vfs = LocalVfs::open(path.to_path_buf()).map_err(std::io::Error::from)?;
    let caps = local_vfs.caps();
    if caps.exclusive_staged_file_publish != Support::Yes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "version mutation requires exclusive staged-file publication for its root lock",
        ));
    }
    let root = local_vfs.local_root().clone();
    let vfs: Arc<dyn Vfs> = Arc::new(local_vfs);
    let lease = RootLock::acquire_vfs(&vfs)?;
    Ok(LockedVersionRoot { root, lease, caps })
}

fn check_version_lease(lease: &RootLock) -> std::io::Result<()> {
    if lease.verify_lease_identity() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "version mutation stopped because root-lock ownership was lost",
        ))
    }
}

/// Keep the newest `keep` versions and delete the rest. Returns the ids of the deleted versions.
pub fn prune(root: &Path, keep: usize) -> std::io::Result<Vec<String>> {
    let locked = acquire_version_root(root)?;
    let mut versions = list_from_local_root(&locked.root)?;
    if versions.is_empty() {
        return Ok(Vec::new());
    }
    versions.sort_by_key(|line| line.ts_ms);
    let prune_count = versions.len().saturating_sub(keep);
    let pruned: Vec<VersionIndexEntry> = versions.drain(..prune_count).collect();
    if pruned.is_empty() {
        return Ok(Vec::new());
    }
    let index_path = relative_path(format!(
        "{}/index.jsonl",
        crate::foundation::names::VERSION_STORE_DIR
    ))?;
    check_version_lease(&locked.lease)?;
    let mut staged = locked.root.create_staged(&index_path)?;
    for line in &versions {
        writeln!(staged, "{}", serde_json::to_string(line)?)?;
    }
    staged.seal(true)?;
    check_version_lease(&locked.lease)?;
    // Publish the logical deletion first: interruption may leave an unindexed orphan directory,
    // but the index never promises a version whose payload was already removed.
    staged.commit()?;

    for entry in &pruned {
        let version_directory = relative_directory(format!(
            "{}/{}",
            crate::foundation::names::VERSION_STORE_DIR,
            entry.id.as_str()
        ))?;
        check_version_lease(&locked.lease)?;
        match locked.root.remove_directory_all(&version_directory) {
            Ok(()) => {
                locked.root.sync_parent(&index_path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "version {:?} was removed from the index, but its orphaned directory could not be deleted ({error})",
                        entry.id.as_str()
                    ),
                ))
            }
        }
    }
    Ok(pruned
        .into_iter()
        .map(|entry| entry.id.into_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "syncdash-version-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn whole_entry(rel: &str) -> PreservedEntry {
        PreservedEntry {
            relative_path: RootRelativePath::try_from(rel).unwrap(),
            payload_kind: VersionPayloadKind::Whole,
            reason: "test".to_owned(),
            old_hash: blake3::hash(b"archived").to_hex().to_string(),
            old_size: 8,
            old_mtime_ms: 0,
            old_mode: None,
            new_hash: None,
            recipe: None,
        }
    }

    #[test]
    fn rdelta_roundtrip_bytes() {
        let old = vec![7u8; 6 * 1024 * 1024];
        let mut new = old.clone();
        for byte in new.iter_mut().take(3_004_096).skip(3_000_000) {
            *byte = 9;
        }
        new.extend_from_slice(&[1u8; 2048]);

        let new_chunks = crate::fs::chunk::chunk_bytes(&new);
        let mut replacement_chunk_locations: std::collections::HashMap<&str, (u64, u32)> =
            std::collections::HashMap::new();
        for chunk in &new_chunks {
            replacement_chunk_locations
                .entry(chunk.hash.as_str())
                .or_insert((chunk.off, chunk.len));
        }
        let mut blob: Vec<u8> = Vec::new();
        let mut recipe: Vec<RecipeStep> = Vec::new();
        for chunk in crate::fs::chunk::chunk_bytes(&old) {
            if let Some(&(replacement_offset, replacement_length)) =
                replacement_chunk_locations.get(chunk.hash.as_str())
            {
                recipe.push(RecipeStep {
                    s: "base".into(),
                    off: replacement_offset,
                    len: replacement_length,
                });
            } else {
                let off = blob.len() as u64;
                blob.extend_from_slice(
                    &old[chunk.off as usize..(chunk.off + chunk.len as u64) as usize],
                );
                recipe.push(RecipeStep {
                    s: "blob".into(),
                    off,
                    len: chunk.len,
                });
            }
        }
        assert!(
            blob.len() < old.len() / 4,
            "rdelta blob should be much smaller (got {} of {})",
            blob.len(),
            old.len()
        );

        let mut rebuilt: Vec<u8> = Vec::new();
        for st in &recipe {
            let (off, len) = (st.off as usize, st.len as usize);
            let s = if st.s == "base" {
                &new[off..off + len]
            } else {
                &blob[off..off + len]
            };
            rebuilt.extend_from_slice(s);
        }
        assert_eq!(blake3::hash(&rebuilt), blake3::hash(&old));
    }

    #[test]
    fn malformed_index_data_is_not_silently_ignored() {
        let root = test_root("bad-index");
        let store = root.join(crate::foundation::names::VERSION_STORE_DIR);
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("index.jsonl"), b"not-json\n").unwrap();
        let error = match list(&root) {
            Ok(_) => panic!("malformed index data must fail"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn typed_version_fields_keep_the_existing_disk_shape() {
        let manifest: VersionManifest = serde_json::from_str(
            r#"{"id":"v1","ts_ms":1,"host":"test","entries":[{"rel":"a.txt","kind":"whole","why":"deleted","old_hash":"","old_size":0,"old_mtime_ms":0}]}"#,
        )
        .unwrap();
        assert_eq!(manifest.id.as_str(), "v1");
        assert_eq!(manifest.entries[0].relative_path.as_str(), "a.txt");
        assert_eq!(manifest.entries[0].payload_kind, VersionPayloadKind::Whole);
        assert_eq!(manifest.entries[0].reason, "deleted");

        let encoded = serde_json::to_value(&manifest).unwrap();
        assert_eq!(encoded["id"], "v1");
        assert_eq!(encoded["entries"][0]["rel"], "a.txt");
        assert_eq!(encoded["entries"][0]["kind"], "whole");
        assert_eq!(encoded["entries"][0]["why"], "deleted");
    }

    #[test]
    fn finish_never_deletes_an_unindexed_displaced_payload() {
        let root = test_root("unindexed-payload");
        let local_root = LocalRoot::open(root.clone()).unwrap();
        let mut writer = VersionWriter::begin(&local_root, || Ok(())).unwrap();
        let version_directory = root.join(to_native(writer.version_dir.as_str()));
        let recovery_file = version_directory.join("recover-me");
        std::fs::write(&recovery_file, b"original").unwrap();
        writer.has_unindexed_payload = true;

        assert!(writer.finish(&[], true, || Ok(())).is_err());
        assert_eq!(std::fs::read(recovery_file).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn preserve_never_replaces_an_existing_version_payload() {
        let root = test_root("preserve-collision");
        let old = root.join("kept.txt");
        std::fs::write(&old, b"original").unwrap();
        let local_root = LocalRoot::open(root.clone()).unwrap();
        let mut writer = VersionWriter::begin(&local_root, || Ok(())).unwrap();
        let retained = root
            .join(to_native(writer.version_dir.as_str()))
            .join("files/kept.txt");
        std::fs::create_dir_all(retained.parent().unwrap()).unwrap();
        std::fs::write(&retained, b"existing history").unwrap();
        let rel = RootRelativePath::new("kept.txt").unwrap();

        let error = writer
            .preserve(&rel, None, "test", false, || Ok(()))
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(old).unwrap(), b"original");
        assert_eq!(std::fs::read(retained).unwrap(), b"existing history");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finish_propagates_index_publication_failure() {
        let root = test_root("finish-failure");
        let old = root.join("kept.txt");
        std::fs::write(&old, b"original").unwrap();
        let local_root = LocalRoot::open(root.clone()).unwrap();
        let mut writer = VersionWriter::begin(&local_root, || Ok(())).unwrap();
        let rel = RootRelativePath::new("kept.txt").unwrap();
        writer
            .preserve(&rel, None, "test", false, || Ok(()))
            .unwrap();
        std::fs::create_dir(
            root.join(crate::foundation::names::VERSION_STORE_DIR)
                .join("index.jsonl"),
        )
        .unwrap();

        assert!(writer.finish(&[], false, || Ok(())).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prune_validates_every_id_before_deleting_anything() {
        let root = test_root("prune-traversal");
        let outside = root.parent().unwrap().join(format!(
            "syncdash-version-prune-sentinel-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&outside);
        std::fs::write(&outside, b"keep").unwrap();
        let store = root.join(crate::foundation::names::VERSION_STORE_DIR);
        std::fs::create_dir_all(&store).unwrap();
        let malicious_id = format!("../../{}", outside.file_name().unwrap().to_string_lossy());
        std::fs::write(
            store.join("index.jsonl"),
            format!(
                "{{\"id\":{},\"ts_ms\":1,\"host\":\"test\",\"ops\":1,\"preserved\":1,\"bytes\":4}}\n",
                serde_json::to_string(&malicious_id).unwrap()
            ),
        )
        .unwrap();

        assert!(prune(&root, 0).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restore_validates_the_whole_manifest_before_mutating_destinations() {
        let root = test_root("restore-traversal");
        std::fs::write(root.join("kept.txt"), b"current").unwrap();
        let version_dir = root
            .join(crate::foundation::names::VERSION_STORE_DIR)
            .join("v1");
        std::fs::create_dir_all(version_dir.join("files")).unwrap();
        std::fs::write(version_dir.join("files/kept.txt"), b"archived").unwrap();
        let mut valid_entry = serde_json::to_value(whole_entry("kept.txt")).unwrap();
        let mut malicious_entry = valid_entry.clone();
        malicious_entry["rel"] = serde_json::Value::String("../outside.txt".to_owned());
        let manifest = serde_json::json!({
            "id": "v1",
            "ts_ms": 1,
            "host": "test",
            "entries": [valid_entry.take(), malicious_entry],
        });
        std::fs::write(
            version_dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        assert!(restore(&root, "v1", &[], false).is_err());
        assert_eq!(std::fs::read(root.join("kept.txt")).unwrap(), b"current");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restore_refuses_an_unsafe_version_selector() {
        let root = test_root("version-selector");
        let error = restore(&root, "../v1", &[], true).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(root);
    }
}
