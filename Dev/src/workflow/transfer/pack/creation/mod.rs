//! Package creation in protocol order: plan, ordered payload members, then manifest.

mod payload;

use std::io::{Read, Seek, Write};
use std::path::Path;

use crate::foundation::path::RootRelativePath;
use crate::foundation::time::now_ms;
use crate::model::chunk::RecipeStep;
use crate::model::plan::{Action, Op, Plan, PlanHeader, Side};
use crate::transfer::pack::format::{
    package_error, Manifest, PackSummary, PayloadEntry, PACK_SCHEMA, PACK_VERSION,
};

use payload::{verify_source_evidence, DeltaBlobReader, EvidenceReader, SourceEvidence};

fn tar_header(size: u64) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(0o644);
    header.set_mtime(now_ms() / 1000);
    header.set_cksum();
    header
}

/// Pack the target-side operations of a plan.
///
/// The source root is opened once and retained for every payload read. `peer_chunks` contains the
/// FastCDC tables for large files already present on the receiver.
pub fn pack(
    plan: &Plan,
    source_root_path: &Path,
    out: &Path,
    peer_chunks: Option<&std::collections::HashMap<String, crate::model::chunk::FileChunks>>,
) -> std::io::Result<PackSummary> {
    let output = std::fs::File::create(out)?;
    pack_to_open_file(plan, source_root_path, output, peer_chunks)
}

/// Write a package through an already-open file whose namespace ownership the caller established.
pub(crate) fn pack_to_open_file(
    plan: &Plan,
    source_root_path: &Path,
    output: std::fs::File,
    peer_chunks: Option<&std::collections::HashMap<String, crate::model::chunk::FileChunks>>,
) -> std::io::Result<PackSummary> {
    let source_root = crate::fs::local_root::LocalRoot::open(source_root_path.to_path_buf())?;
    let target_ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|operation| operation.side == Side::Target && operation.action.is_executable())
        .cloned()
        .collect();
    let skipped_source_side = plan
        .ops
        .iter()
        .filter(|operation| operation.side == Side::Source)
        .count();
    if skipped_source_side > 0 {
        crate::log_info!("pack", "note: {skipped_source_side} source-side op(s) not packed (they run locally, not on the peer)");
    }
    for operation in &target_ops {
        RootRelativePath::try_from(operation.path.as_str())
            .map_err(|error| package_error(format!("invalid plan path: {error}")))?;
        if let Some(source) = operation.from.as_deref() {
            RootRelativePath::try_from(source)
                .map_err(|error| package_error(format!("invalid plan source path: {error}")))?;
        }
    }

    let subplan = Plan {
        header: PlanHeader {
            op_count: target_ops.len() as u64,
            ..plan.header.clone()
        },
        ops: target_ops.clone(),
    };
    let mut plan_bytes = Vec::new();
    subplan.write_to(&mut plan_bytes)?;
    let plan_hash = blake3::hash(&plan_bytes).to_hex().to_string();

    let mut archive = tar::Builder::new(std::io::BufWriter::new(output));
    archive.append_data(
        &mut tar_header(plan_bytes.len() as u64),
        "plan.jsonl",
        &plan_bytes[..],
    )?;

    // Every regular Copy/Update has exactly one payload; eligible large updates use FastCDC delta.
    let mut seen = std::collections::HashSet::new();
    let mut payload = Vec::new();
    let mut bytes_total = 0u64;
    let mut delta_saved = 0u64;
    for operation in &target_ops {
        if !matches!(operation.action, Action::Copy | Action::Update) {
            continue;
        }
        if operation.link.is_some() {
            continue;
        }
        if !seen.insert(operation.path.clone()) {
            return Err(package_error(format!(
                "plan requests the payload {:?} more than once",
                operation.path
            )));
        }
        let relative = RootRelativePath::try_from(operation.path.as_str()).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        let mut source_file = source_root.open_read(&relative)?;
        let metadata = source_file.metadata()?;
        let size = metadata.len();
        let mtime_ms = crate::foundation::time::meta_mtime_ms(&metadata);
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            Some(metadata.mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let mode: Option<u32> = None;

        let base = peer_chunks
            .and_then(|chunks| chunks.get(&operation.path))
            .filter(|_| size >= crate::model::chunk::DELTA_MIN_SIZE);
        if let Some(base) = base {
            let mut base_chunks: std::collections::HashMap<(&str, u32), u64> =
                std::collections::HashMap::new();
            for chunk in &base.chunks {
                base_chunks
                    .entry((chunk.hash.as_str(), chunk.len))
                    .or_insert(chunk.off);
            }
            let mut blob_size = 0u64;
            let mut blob_hasher = blake3::Hasher::new();
            let mut sampled_hasher = crate::pipeline::scan::digest::SampledDigestBuilder::new(size);
            let mut recipe: Vec<RecipeStep> = Vec::new();
            let summary = crate::fs::chunk::visit_chunks(&mut source_file, |chunk, bytes| {
                sampled_hasher.update(chunk.off, bytes);
                if let Some(base_offset) =
                    base_chunks.get(&(chunk.hash.as_str(), chunk.len)).copied()
                {
                    recipe.push(RecipeStep {
                        s: "base".into(),
                        off: base_offset,
                        len: chunk.len,
                    });
                } else {
                    let blob_offset = blob_size;
                    blob_size = blob_size
                        .checked_add(chunk.len as u64)
                        .ok_or_else(|| package_error("delta blob length overflow"))?;
                    blob_hasher.update(bytes);
                    recipe.push(RecipeStep {
                        s: "blob".into(),
                        off: blob_offset,
                        len: chunk.len,
                    });
                }
                Ok(())
            })?;
            let evidence = SourceEvidence {
                size: summary.size,
                full_hash: summary.hash,
                sampled_hash: format!("~{}", sampled_hasher.finish()),
            };
            verify_source_evidence(operation, size, mtime_ms, mode, &evidence)?;
            let blob_hash = blob_hasher.finalize().to_hex().to_string();
            source_file.seek(std::io::SeekFrom::Start(0))?;
            let mut blob_reader = DeltaBlobReader::new(&mut source_file, &base_chunks, blob_size);
            archive.append_data(
                &mut tar_header(blob_size),
                format!("payload/{relative}"),
                &mut blob_reader,
            )?;
            blob_reader.verify(size, &evidence.full_hash, &blob_hash)?;
            bytes_total += blob_size;
            delta_saved += size.saturating_sub(blob_size);
            payload.push(PayloadEntry {
                rel: relative,
                size,
                mtime_ms,
                hash: evidence.full_hash,
                mode,
                kind: "delta".into(),
                base_hash: Some(base.hash.clone()),
                blob_hash: Some(blob_hash),
                recipe: Some(recipe),
            });
        } else {
            let evidence = {
                let mut reader =
                    EvidenceReader::new(std::io::BufReader::new(&mut source_file), size);
                archive.append_data(
                    &mut tar_header(size),
                    format!("payload/{relative}"),
                    &mut reader,
                )?;
                let mut extra = [0u8; 1];
                if reader.read(&mut extra)? != 0 {
                    return Err(package_error(format!(
                        "source file grew while packing {}",
                        relative.as_str()
                    )));
                }
                reader.finish()
            };
            verify_source_evidence(operation, size, mtime_ms, mode, &evidence)?;
            bytes_total += size;
            payload.push(PayloadEntry {
                rel: relative,
                size,
                mtime_ms,
                hash: evidence.full_hash,
                mode,
                kind: "whole".into(),
                base_hash: None,
                blob_hash: None,
                recipe: None,
            });
        }
    }

    let mut combined = blake3::Hasher::new();
    for entry in &payload {
        combined.update(entry.hash.as_bytes());
    }
    let manifest = Manifest {
        schema: PACK_SCHEMA,
        pack_version: PACK_VERSION,
        created_ms: now_ms(),
        source_host: crate::model::table::host_name(),
        source_os: std::env::consts::OS.to_string(),
        target_root_hint: plan.header.target_root.clone(),
        op_count: target_ops.len() as u64,
        plan_blake3: plan_hash,
        payload,
        payload_combined_blake3: combined.finalize().to_hex().to_string(),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    archive.append_data(
        &mut tar_header(manifest_bytes.len() as u64),
        "manifest.json",
        &manifest_bytes[..],
    )?;
    archive.into_inner()?.flush()?;

    Ok(PackSummary {
        ops: manifest.op_count,
        files: manifest.payload.len() as u64,
        bytes: bytes_total,
        delta_saved,
    })
}
