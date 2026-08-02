//! Verified payload extraction and delta reconstruction in an isolated temporary directory.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::foundation::path::{to_native, RootRelativePath};
use crate::foundation::time::now_ms;
use crate::transfer::pack::format::{package_error, Manifest};

/// Read granularity for hashing a delta base off the target root.
const READ_CHUNK: u64 = 8 * 1024 * 1024;

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn create_staging_dir() -> std::io::Result<PathBuf> {
    for _ in 0..16 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "syncdash-staging-{}-{}-{sequence}",
            std::process::id(),
            now_ms()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique package staging directory",
    ))
}

pub(super) struct StagingDir(PathBuf);

impl StagingDir {
    fn create() -> std::io::Result<Self> {
        create_staging_dir().map(Self)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn stage_payloads(
    package_file: &mut std::fs::File,
    manifest: &Manifest,
    target_root: &Path,
    verbose: bool,
) -> std::io::Result<StagingDir> {
    let staging = StagingDir::create()?;
    let by_rel: std::collections::HashMap<&str, _> = manifest
        .payload
        .iter()
        .map(|payload| (payload.rel.as_str(), payload))
        .collect();
    let mut extract_errors = 0u64;
    let mut extracted = std::collections::HashSet::new();

    package_file.seek(SeekFrom::Start(0))?;
    let mut archive = tar::Archive::new(std::io::BufReader::new(package_file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = path.to_str().ok_or_else(|| {
            package_error("package contains a member whose path is not valid UTF-8")
        })?;
        let Some(relative) = name.strip_prefix("payload/") else {
            continue;
        };
        let relative = RootRelativePath::try_from(relative)
            .map_err(|error| package_error(format!("unsafe tar payload path: {error}")))?;
        if !extracted.insert(relative.clone()) {
            return Err(package_error(format!(
                "package payload {:?} changed or repeated during extraction",
                relative.as_str()
            )));
        }
        let metadata = by_rel.get(relative.as_str()).ok_or_else(|| {
            package_error(format!(
                "package payload {:?} changed after structural validation",
                relative.as_str()
            ))
        })?;
        let destination = staging.path().join(to_native(relative.as_str()));
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if metadata.kind == "delta" {
            // Verify the blob, verify the base, reconstruct the recipe, then verify the result.
            let mut blob = Vec::new();
            entry.read_to_end(&mut blob)?;
            let actual_blob_hash = blake3::hash(&blob).to_hex().to_string();
            if metadata.blob_hash.as_deref() != Some(actual_blob_hash.as_str()) {
                extract_errors += 1;
                crate::log_error!("pack", "ERR  delta blob hash mismatch: {relative}");
                continue;
            }
            let base_path = target_root.join(to_native(relative.as_str()));
            // One handle hashes the base and then serves absolute recipe seeks.
            let mut base_hasher = blake3::Hasher::new();
            let opened = (|| -> std::io::Result<std::fs::File> {
                let mut file = std::fs::File::open(&base_path)?;
                let mut buffer = vec![0u8; file.metadata()?.len().clamp(1, READ_CHUNK) as usize];
                loop {
                    let length = file.read(&mut buffer)?;
                    if length == 0 {
                        break;
                    }
                    base_hasher.update(&buffer[..length]);
                }
                Ok(file)
            })();
            let Ok(mut base_file) = opened else {
                extract_errors += 1;
                crate::log_error!("pack", "ERR  delta base missing/unreadable: {relative}");
                continue;
            };
            let actual_base_hash = base_hasher.finalize().to_hex().to_string();
            if metadata.base_hash.as_deref() != Some(actual_base_hash.as_str()) {
                extract_errors += 1;
                crate::log_error!(
                    "pack",
                    "ERR  delta base changed since chunk table: {relative} — rerun to repack"
                );
                continue;
            }

            let mut output = std::fs::File::create(&destination)?;
            let mut file_hasher = blake3::Hasher::new();
            let mut buffer = vec![0u8; 1 << 16];
            let mut valid = true;
            if let Some(recipe) = &metadata.recipe {
                'steps: for step in recipe {
                    match step.s.as_str() {
                        "base" => {
                            base_file.seek(SeekFrom::Start(step.off))?;
                            let mut remaining = step.len as usize;
                            while remaining > 0 {
                                let wanted = remaining.min(buffer.len());
                                let length = base_file.read(&mut buffer[..wanted])?;
                                if length == 0 {
                                    valid = false;
                                    break 'steps;
                                }
                                file_hasher.update(&buffer[..length]);
                                output.write_all(&buffer[..length])?;
                                remaining -= length;
                            }
                        }
                        "blob" => {
                            let start = step.off as usize;
                            let Some(end) = start.checked_add(step.len as usize) else {
                                valid = false;
                                break 'steps;
                            };
                            if end > blob.len() {
                                valid = false;
                                break 'steps;
                            }
                            file_hasher.update(&blob[start..end]);
                            output.write_all(&blob[start..end])?;
                        }
                        _ => unreachable!("recipe sources were structurally validated"),
                    }
                }
            }
            let final_hash = file_hasher.finalize().to_hex().to_string();
            if !valid || final_hash != metadata.hash {
                extract_errors += 1;
                crate::log_error!("pack", "ERR  delta reconstruction failed: {relative}");
                let _ = std::fs::remove_file(&destination);
            } else if verbose {
                println!(
                    "OK   delta reconstructed: {relative} (blob {} B of {} B)",
                    blob.len(),
                    metadata.size
                );
            }
        } else {
            let mut output = std::fs::File::create(&destination)?;
            let mut hasher = blake3::Hasher::new();
            let mut buffer = [0u8; 1 << 16];
            let mut written = 0u64;
            loop {
                let length = entry.read(&mut buffer)?;
                if length == 0 {
                    break;
                }
                hasher.update(&buffer[..length]);
                output.write_all(&buffer[..length])?;
                written += length as u64;
            }
            let actual_hash = hasher.finalize().to_hex().to_string();
            if metadata.hash == actual_hash && written == metadata.size {
                if verbose {
                    println!("OK   payload verified: {relative}");
                }
            } else {
                extract_errors += 1;
                crate::log_error!(
                    "pack",
                    "ERR  payload hash mismatch: {relative} (manifest {} vs {actual_hash})",
                    metadata.hash
                );
                let _ = std::fs::remove_file(&destination);
            }
        }
    }

    let expected: std::collections::HashSet<_> = manifest
        .payload
        .iter()
        .map(|payload| payload.rel.clone())
        .collect();
    if extracted != expected {
        return Err(package_error(
            "package payload set changed after structural validation",
        ));
    }
    if extract_errors > 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{extract_errors} payload file(s) failed verification — nothing applied"),
        ));
    }
    Ok(staging)
}
