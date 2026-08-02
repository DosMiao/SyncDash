//! Tar package transfer for target-side plan operations.
//!
//! A package contains one `plan.jsonl`, one `manifest.json`, and exactly one payload member for
//! each regular Copy/Update. Apply validates the complete structure and digest graph before it
//! extracts or mutates the target, then delegates execution to the normal apply pipeline.

use crate::foundation::path::{to_native, RootRelativePath};
use crate::foundation::time::now_ms;
use crate::model::chunk::RecipeStep;
use crate::model::plan::{Action, Op, Plan, PlanHeader, Side};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const PACK_VERSION: u32 = 2;
const PACK_SCHEMA: u32 = 1;

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
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique package staging directory",
    ))
}

struct StagingDir(PathBuf);

impl StagingDir {
    fn create() -> std::io::Result<Self> {
        create_staging_dir().map(Self)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PayloadEntry {
    pub rel: RootRelativePath,
    pub size: u64,
    pub mtime_ms: i64,
    /// blake3 of the final whole file (checked after reassembly and after extraction alike)
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<u32>,
    /// "whole" (default) | "delta"
    #[serde(default = "default_kind")]
    pub kind: String,
    /// delta: the blake3 the target's existing file must match
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_hash: Option<String>,
    /// delta: blake3 of the tar's payload/<rel> entry (= the delta blob)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub blob_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub recipe: Option<Vec<RecipeStep>>,
}

fn default_kind() -> String {
    "whole".into()
}

#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    #[serde(default)]
    pub pack_version: u32,
    pub created_ms: u64,
    pub source_host: String,
    pub source_os: String,
    pub target_root_hint: String,
    pub op_count: u64,
    pub plan_blake3: String,
    pub payload: Vec<PayloadEntry>,
    pub payload_combined_blake3: String,
}

struct SourceEvidence {
    size: u64,
    full_hash: String,
    sampled_hash: String,
}

struct EvidenceReader<R: Read> {
    inner: R,
    full_hash: blake3::Hasher,
    sampled_hash: crate::pipeline::scan::digest::SampledDigestBuilder,
    offset: u64,
}

impl<R: Read> EvidenceReader<R> {
    fn finish(self) -> SourceEvidence {
        SourceEvidence {
            size: self.offset,
            full_hash: self.full_hash.finalize().to_hex().to_string(),
            sampled_hash: format!("~{}", self.sampled_hash.finish()),
        }
    }
}

impl<R: Read> Read for EvidenceReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.full_hash.update(&buf[..n]);
        self.sampled_hash.update(self.offset, &buf[..n]);
        self.offset = self
            .offset
            .checked_add(n as u64)
            .ok_or_else(|| package_error("source file length overflow"))?;
        Ok(n)
    }
}

struct DeltaBlobReader<'base, R: Read> {
    chunks: crate::fs::chunk::ChunkStream<R>,
    base_chunks: &'base std::collections::HashMap<(&'base str, u32), u64>,
    expected_blob_size: u64,
    current: Vec<u8>,
    current_offset: usize,
    file_size: u64,
    file_hash: blake3::Hasher,
    blob_size: u64,
    blob_hash: blake3::Hasher,
    finished: bool,
}

impl<'base, R: Read> DeltaBlobReader<'base, R> {
    fn new(
        reader: R,
        base_chunks: &'base std::collections::HashMap<(&'base str, u32), u64>,
        expected_blob_size: u64,
    ) -> Self {
        Self {
            chunks: crate::fs::chunk::stream_chunks(reader),
            base_chunks,
            expected_blob_size,
            current: Vec::new(),
            current_offset: 0,
            file_size: 0,
            file_hash: blake3::Hasher::new(),
            blob_size: 0,
            blob_hash: blake3::Hasher::new(),
            finished: false,
        }
    }

    fn verify(
        self,
        expected_file_size: u64,
        expected_file_hash: &str,
        expected_blob_hash: &str,
    ) -> std::io::Result<()> {
        if !self.finished
            || self.file_size != expected_file_size
            || self.file_hash.finalize().to_hex().as_str() != expected_file_hash
        {
            return Err(package_error(
                "source file changed while its delta payload was being packed",
            ));
        }
        if self.blob_size != self.expected_blob_size
            || self.blob_hash.finalize().to_hex().as_str() != expected_blob_hash
        {
            return Err(package_error(
                "source file changed which chunks belong in its delta payload",
            ));
        }
        Ok(())
    }
}

impl<R: Read> Read for DeltaBlobReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            if self.current_offset < self.current.len() {
                let available = &self.current[self.current_offset..];
                let length = available.len().min(output.len());
                output[..length].copy_from_slice(&available[..length]);
                self.current_offset += length;
                return Ok(length);
            }
            match self.chunks.next() {
                Some(Ok(chunk)) => {
                    self.file_size = self
                        .file_size
                        .checked_add(chunk.info.len as u64)
                        .ok_or_else(|| package_error("source file length overflow"))?;
                    self.file_hash.update(&chunk.bytes);
                    if self
                        .base_chunks
                        .contains_key(&(chunk.info.hash.as_str(), chunk.info.len))
                    {
                        continue;
                    }
                    let blob_size = self
                        .blob_size
                        .checked_add(chunk.info.len as u64)
                        .ok_or_else(|| package_error("delta blob length overflow"))?;
                    if blob_size > self.expected_blob_size {
                        return Err(package_error(
                            "source file gained unmatched chunks while its delta payload was being packed",
                        ));
                    }
                    self.blob_size = blob_size;
                    self.blob_hash.update(&chunk.bytes);
                    self.current = chunk.bytes;
                    self.current_offset = 0;
                }
                Some(Err(error)) => return Err(error),
                None => {
                    self.finished = true;
                    return Ok(0);
                }
            }
        }
    }
}

fn tar_header(size: u64) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_size(size);
    h.set_mode(0o644);
    h.set_mtime(now_ms() / 1000);
    h.set_cksum();
    h
}

pub struct PackSummary {
    pub ops: u64,
    pub files: u64,
    pub bytes: u64,
    /// Bytes saved by delta transfer (whole-file size − the blob actually packed)
    pub delta_saved: u64,
}

fn package_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn verify_source_evidence(
    operation: &Op,
    size: u64,
    mtime_ms: i64,
    mode: Option<u32>,
    evidence: &SourceEvidence,
) -> std::io::Result<()> {
    if evidence.size != size || operation.size.is_some_and(|expected| expected != size) {
        return Err(package_error(format!(
            "source file changed size after Compare: {}",
            operation.path
        )));
    }
    if operation
        .mode
        .is_some_and(|expected| mode != Some(expected))
    {
        return Err(package_error(format!(
            "source file changed permissions after Compare: {}",
            operation.path
        )));
    }
    match operation.hash.as_deref() {
        Some(expected) if expected.starts_with('~') => {
            if evidence.sampled_hash != expected {
                return Err(package_error(format!(
                    "source file content changed after Compare: {}",
                    operation.path
                )));
            }
        }
        Some(expected) if expected != evidence.full_hash => {
            return Err(package_error(format!(
                "source file content changed after Compare: {}",
                operation.path
            )));
        }
        Some(_) => {}
        None => {}
    }
    if operation
        .mtime_ms
        .is_some_and(|expected| expected != mtime_ms)
    {
        return Err(package_error(format!(
            "source file timestamp changed after Compare: {}",
            operation.path
        )));
    }
    Ok(())
}

fn validate_digest(value: &str, label: &str) -> std::io::Result<()> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(package_error(format!(
            "{label} must be a lowercase 64-character BLAKE3 digest"
        )))
    }
}

fn validate_package_structure(
    plan: &Plan,
    manifest: &Manifest,
    tar_payload: &std::collections::HashMap<RootRelativePath, u64>,
) -> std::io::Result<()> {
    if manifest.schema != PACK_SCHEMA {
        return Err(package_error(format!(
            "package table schema {} does not match this build ({})",
            manifest.schema, PACK_SCHEMA
        )));
    }
    if plan.header.kind != "plan" {
        return Err(package_error(format!(
            "package plan has unexpected kind {:?}",
            plan.header.kind
        )));
    }
    if plan.header.op_count != plan.ops.len() as u64 || manifest.op_count != plan.ops.len() as u64 {
        return Err(package_error(format!(
            "package operation counts disagree: plan header={}, manifest={}, decoded={}",
            plan.header.op_count,
            manifest.op_count,
            plan.ops.len()
        )));
    }
    if manifest.target_root_hint != plan.header.target_root {
        return Err(package_error(
            "manifest target root hint does not match the signed package plan",
        ));
    }

    let mut required_payload = std::collections::HashSet::new();
    for op in &plan.ops {
        if op.side != Side::Target || matches!(op.action, Action::Conflict | Action::Note) {
            return Err(package_error(format!(
                "package plan contains an operation that cannot execute on the target: {:?} {:?}",
                op.side, op.action
            )));
        }
        let path = RootRelativePath::try_from(op.path.as_str())
            .map_err(|e| package_error(format!("unsafe path in package plan: {e}")))?;
        if let Some(from) = &op.from {
            RootRelativePath::try_from(from.as_str())
                .map_err(|e| package_error(format!("unsafe source path in package plan: {e}")))?;
        }
        if matches!(op.action, Action::Copy | Action::Update)
            && op.link.is_none()
            && !required_payload.insert(path.clone())
        {
            return Err(package_error(format!(
                "package plan requests the payload {:?} more than once",
                path.as_str()
            )));
        }
    }

    validate_digest(&manifest.plan_blake3, "manifest plan_blake3")?;
    validate_digest(
        &manifest.payload_combined_blake3,
        "manifest payload_combined_blake3",
    )?;

    let mut manifest_payload = std::collections::HashSet::new();
    let mut combined = blake3::Hasher::new();
    for payload in &manifest.payload {
        if !manifest_payload.insert(payload.rel.clone()) {
            return Err(package_error(format!(
                "manifest contains duplicate payload {:?}",
                payload.rel.as_str()
            )));
        }
        validate_digest(
            &payload.hash,
            &format!("payload {:?} hash", payload.rel.as_str()),
        )?;
        combined.update(payload.hash.as_bytes());
        let tar_size = tar_payload.get(&payload.rel).ok_or_else(|| {
            package_error(format!(
                "manifest payload {:?} is missing from the tar archive",
                payload.rel.as_str()
            ))
        })?;
        match payload.kind.as_str() {
            "whole"
                if payload.base_hash.is_none()
                    && payload.blob_hash.is_none()
                    && payload.recipe.is_none() =>
            {
                if *tar_size != payload.size {
                    return Err(package_error(format!(
                        "whole payload {:?} has tar size {} but manifest size {}",
                        payload.rel.as_str(),
                        tar_size,
                        payload.size
                    )));
                }
            }
            "delta"
                if payload.base_hash.is_some()
                    && payload.blob_hash.is_some()
                    && payload.recipe.is_some() =>
            {
                validate_digest(
                    payload.base_hash.as_deref().unwrap(),
                    &format!("payload {:?} base_hash", payload.rel.as_str()),
                )?;
                validate_digest(
                    payload.blob_hash.as_deref().unwrap(),
                    &format!("payload {:?} blob_hash", payload.rel.as_str()),
                )?;
                let mut reconstructed_size = 0u64;
                for step in payload.recipe.as_ref().unwrap() {
                    reconstructed_size = reconstructed_size
                        .checked_add(step.len as u64)
                        .ok_or_else(|| package_error("delta recipe size overflow"))?;
                    let end = step
                        .off
                        .checked_add(step.len as u64)
                        .ok_or_else(|| package_error("delta recipe range overflow"))?;
                    match step.s.as_str() {
                        "base" => {}
                        "blob" if end <= *tar_size => {}
                        "blob" => {
                            return Err(package_error(format!(
                                "delta recipe for {:?} reads beyond its blob",
                                payload.rel.as_str()
                            )))
                        }
                        other => {
                            return Err(package_error(format!(
                                "delta recipe for {:?} has unknown source {other:?}",
                                payload.rel.as_str()
                            )))
                        }
                    }
                }
                if reconstructed_size != payload.size {
                    return Err(package_error(format!(
                        "delta recipe for {:?} reconstructs {} bytes, expected {}",
                        payload.rel.as_str(),
                        reconstructed_size,
                        payload.size
                    )));
                }
            }
            "whole" => {
                return Err(package_error(format!(
                    "whole payload {:?} carries delta-only fields",
                    payload.rel.as_str()
                )))
            }
            "delta" => {
                return Err(package_error(format!(
                    "delta payload {:?} is structurally incomplete",
                    payload.rel.as_str()
                )))
            }
            other => {
                return Err(package_error(format!(
                    "payload {:?} has unknown kind {other:?}",
                    payload.rel.as_str()
                )))
            }
        }
    }

    let combined = combined.finalize().to_hex().to_string();
    if combined != manifest.payload_combined_blake3 {
        return Err(package_error(format!(
            "payload combined digest mismatch: manifest {} vs actual {combined}",
            manifest.payload_combined_blake3
        )));
    }
    if required_payload != manifest_payload {
        let missing = required_payload
            .difference(&manifest_payload)
            .map(RootRelativePath::as_str)
            .collect::<Vec<_>>();
        let extra = manifest_payload
            .difference(&required_payload)
            .map(RootRelativePath::as_str)
            .collect::<Vec<_>>();
        return Err(package_error(format!(
            "plan and manifest payload sets disagree (missing={missing:?}, extra={extra:?})"
        )));
    }
    let tar_set: std::collections::HashSet<_> = tar_payload.keys().cloned().collect();
    if tar_set != manifest_payload {
        let missing = manifest_payload
            .difference(&tar_set)
            .map(RootRelativePath::as_str)
            .collect::<Vec<_>>();
        let extra = tar_set
            .difference(&manifest_payload)
            .map(RootRelativePath::as_str)
            .collect::<Vec<_>>();
        return Err(package_error(format!(
            "manifest and tar payload sets disagree (missing={missing:?}, extra={extra:?})"
        )));
    }
    Ok(())
}

/// Packs the target-side operations of a plan.
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

/// Writes a package through an already-open file whose namespace ownership the caller established.
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
        .filter(|o| o.side == Side::Target && !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    let skipped_source_side = plan.ops.iter().filter(|o| o.side == Side::Source).count();
    if skipped_source_side > 0 {
        crate::log_info!("pack", "note: {skipped_source_side} source-side op(s) not packed (they run locally, not on the peer)");
    }
    for op in &target_ops {
        RootRelativePath::try_from(op.path.as_str())
            .map_err(|error| package_error(format!("invalid plan path: {error}")))?;
        if let Some(source) = op.from.as_deref() {
            RootRelativePath::try_from(source)
                .map_err(|error| package_error(format!("invalid plan source path: {error}")))?;
        }
    }

    // Sub-plan: same header, target-side ops only
    let sub = Plan {
        header: PlanHeader {
            op_count: target_ops.len() as u64,
            ..plan.header.clone()
        },
        ops: target_ops.clone(),
    };
    let mut plan_bytes: Vec<u8> = Vec::new();
    sub.write_to(&mut plan_bytes)?;
    let plan_hash = blake3::hash(&plan_bytes).to_hex().to_string();

    let mut tarb = tar::Builder::new(std::io::BufWriter::new(output));
    tarb.append_data(
        &mut tar_header(plan_bytes.len() as u64),
        "plan.jsonl",
        &plan_bytes[..],
    )?;

    // Every regular Copy/Update has exactly one payload; eligible large updates use FastCDC delta.
    let mut seen = std::collections::HashSet::new();
    let mut payload: Vec<PayloadEntry> = Vec::new();
    let mut bytes_total = 0u64;
    let mut delta_saved = 0u64;
    for op in &target_ops {
        if !matches!(op.action, Action::Copy | Action::Update) {
            continue;
        }
        if op.link.is_some() {
            continue; // a symlink has no payload
        }
        if !seen.insert(op.path.clone()) {
            return Err(package_error(format!(
                "plan requests the payload {:?} more than once",
                op.path
            )));
        }
        let rel = RootRelativePath::try_from(op.path.as_str())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let mut source_file = source_root.open_read(&rel)?;
        let md = source_file.metadata()?;
        let size = md.len();
        let mtime_ms = crate::foundation::time::meta_mtime_ms(&md);
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            Some(md.mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let mode: Option<u32> = None;

        let base = peer_chunks
            .and_then(|m| m.get(&op.path))
            .filter(|_| size >= crate::model::chunk::DELTA_MIN_SIZE);
        if let Some(base) = base {
            let mut base_chunks: std::collections::HashMap<(&str, u32), u64> =
                std::collections::HashMap::new();
            for c in &base.chunks {
                base_chunks.entry((c.hash.as_str(), c.len)).or_insert(c.off);
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
            verify_source_evidence(op, size, mtime_ms, mode, &evidence)?;
            let blob_hash = blob_hasher.finalize().to_hex().to_string();
            source_file.seek(std::io::SeekFrom::Start(0))?;
            let mut blob_reader = DeltaBlobReader::new(&mut source_file, &base_chunks, blob_size);
            tarb.append_data(
                &mut tar_header(blob_size),
                format!("payload/{rel}"),
                &mut blob_reader,
            )?;
            blob_reader.verify(size, &evidence.full_hash, &blob_hash)?;
            bytes_total += blob_size;
            delta_saved += size.saturating_sub(blob_size);
            payload.push(PayloadEntry {
                rel,
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
                let mut reader = EvidenceReader {
                    inner: std::io::BufReader::new(&mut source_file),
                    full_hash: blake3::Hasher::new(),
                    sampled_hash: crate::pipeline::scan::digest::SampledDigestBuilder::new(size),
                    offset: 0,
                };
                tarb.append_data(&mut tar_header(size), format!("payload/{rel}"), &mut reader)?;
                let mut extra = [0u8; 1];
                if reader.read(&mut extra)? != 0 {
                    return Err(package_error(format!(
                        "source file grew while packing {}",
                        rel.as_str()
                    )));
                }
                reader.finish()
            };
            verify_source_evidence(op, size, mtime_ms, mode, &evidence)?;
            bytes_total += size;
            payload.push(PayloadEntry {
                rel,
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

    let mut comb = blake3::Hasher::new();
    for p in &payload {
        comb.update(p.hash.as_bytes());
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
        payload_combined_blake3: comb.finalize().to_hex().to_string(),
    };
    let mani_bytes = serde_json::to_vec_pretty(&manifest)?;
    tarb.append_data(
        &mut tar_header(mani_bytes.len() as u64),
        "manifest.json",
        &mani_bytes[..],
    )?;
    tarb.into_inner()?.flush()?;

    Ok(PackSummary {
        ops: manifest.op_count,
        files: manifest.payload.len() as u64,
        bytes: bytes_total,
        delta_saved,
    })
}

/// Execute a package on the far end. dry_run=true only verifies and lists. Returns (done, skipped, errors).
pub fn apply_pack(
    pkg: &Path,
    target_root_override: Option<&Path>,
    do_apply: bool,
    verbose: bool,
    versioning: bool,
) -> std::io::Result<(u64, u64, u64)> {
    use std::io::{Seek, SeekFrom};

    let mut plan_bytes: Option<Vec<u8>> = None;
    let mut manifest: Option<Manifest> = None;
    let mut tar_payload = std::collections::HashMap::new();
    let mut package_file = std::fs::File::open(pkg)?;
    {
        let mut ar = tar::Archive::new(std::io::BufReader::new(&mut package_file));
        for entry in ar.entries()? {
            let mut entry = entry?;
            if !entry.header().entry_type().is_file() {
                return Err(package_error("package members must all be regular files"));
            }
            let path = entry.path()?;
            let name = path.to_str().ok_or_else(|| {
                package_error("package contains a member whose path is not valid UTF-8")
            })?;
            if name == "plan.jsonl" {
                if plan_bytes.is_some() {
                    return Err(package_error(
                        "package contains duplicate plan.jsonl members",
                    ));
                }
                let mut v = Vec::new();
                entry.read_to_end(&mut v)?;
                plan_bytes = Some(v);
            } else if name == "manifest.json" {
                if manifest.is_some() {
                    return Err(package_error(
                        "package contains duplicate manifest.json members",
                    ));
                }
                let mut v = Vec::new();
                entry.read_to_end(&mut v)?;
                manifest = Some(
                    serde_json::from_slice(&v)
                        .map_err(|e| package_error(format!("bad manifest: {e}")))?,
                );
            } else if let Some(rel) = name.strip_prefix("payload/") {
                let rel = RootRelativePath::try_from(rel)
                    .map_err(|e| package_error(format!("unsafe tar payload path: {e}")))?;
                let size = entry.size();
                if tar_payload.insert(rel.clone(), size).is_some() {
                    return Err(package_error(format!(
                        "package contains duplicate payload {:?}",
                        rel.as_str()
                    )));
                }
            } else {
                return Err(package_error(format!(
                    "package contains unexpected member {name:?}"
                )));
            }
        }
    }
    let plan_bytes = plan_bytes.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "package has no plan.jsonl")
    })?;
    let manifest = manifest.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "package has no manifest.json",
        )
    })?;

    if manifest.pack_version > PACK_VERSION {
        return Err(package_error(format!(
            "package version {} newer than this binary supports ({PACK_VERSION}) — rebuild the peer",
            manifest.pack_version
        )));
    }
    if manifest.pack_version < PACK_VERSION {
        return Err(package_error(format!(
            "package version {} is older than this binary requires ({PACK_VERSION}) — regenerate the package",
            manifest.pack_version
        )));
    }
    let got = blake3::hash(&plan_bytes).to_hex().to_string();
    if got != manifest.plan_blake3 {
        return Err(package_error(format!(
            "plan hash mismatch: manifest {} vs actual {got}",
            manifest.plan_blake3
        )));
    }

    let plan = Plan::from_reader(std::io::BufReader::new(&plan_bytes[..]))?;
    validate_package_structure(&plan, &manifest, &tar_payload)?;

    let target_root: PathBuf = match target_root_override {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(&plan.header.target_root),
    };
    println!(
        "package: {} op(s), {} payload file(s), from {} ({}) -> target {}",
        plan.ops.len(),
        manifest.payload.len(),
        manifest.source_host,
        manifest.source_os,
        target_root.display()
    );

    if !do_apply {
        for op in &plan.ops {
            println!("DRY  {:?} {}  ({})", op.action, op.path, op.reason);
        }
        println!("dry-run OK: package structure and metadata verified; rerun with --apply");
        return Ok((0, plan.ops.len() as u64, 0));
    }

    if !target_root.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("target root not accessible: {}", target_root.display()),
        ));
    }

    let staging = StagingDir::create()?;
    let by_rel: std::collections::HashMap<&str, &PayloadEntry> = manifest
        .payload
        .iter()
        .map(|p| (p.rel.as_str(), p))
        .collect();
    let mut extract_errors = 0u64;
    let mut extracted = std::collections::HashSet::new();
    {
        package_file.seek(SeekFrom::Start(0))?;
        let mut ar = tar::Archive::new(std::io::BufReader::new(&mut package_file));
        for entry in ar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;
            let name = path.to_str().ok_or_else(|| {
                package_error("package contains a member whose path is not valid UTF-8")
            })?;
            let Some(rel) = name.strip_prefix("payload/") else {
                continue;
            };
            let rel = RootRelativePath::try_from(rel)
                .map_err(|e| package_error(format!("unsafe tar payload path: {e}")))?;
            if !extracted.insert(rel.clone()) {
                return Err(package_error(format!(
                    "package payload {:?} changed or repeated during extraction",
                    rel.as_str()
                )));
            }
            let meta = by_rel.get(rel.as_str()).ok_or_else(|| {
                package_error(format!(
                    "package payload {:?} changed after structural validation",
                    rel.as_str()
                ))
            })?;
            let dst = staging.path().join(to_native(rel.as_str()));
            if let Some(par) = dst.parent() {
                std::fs::create_dir_all(par)?;
            }
            if meta.kind == "delta" {
                // Delta: verify the blob → verify the base → reassemble per the recipe → verify the result
                let mut blob = Vec::new();
                entry.read_to_end(&mut blob)?;
                let got = blake3::hash(&blob).to_hex().to_string();
                if meta.blob_hash.as_deref() != Some(got.as_str()) {
                    extract_errors += 1;
                    crate::log_error!("pack", "ERR  delta blob hash mismatch: {rel}");
                    continue;
                }
                let base_path = target_root.join(to_native(rel.as_str()));
                // One handle hashes the base and then serves the recipe's seeks — the steps below
                // seek absolutely, so nothing has to rewind it.
                let mut bh = blake3::Hasher::new();
                let opened = (|| -> std::io::Result<std::fs::File> {
                    let mut f = std::fs::File::open(&base_path)?;
                    let mut hbuf = vec![0u8; f.metadata()?.len().clamp(1, READ_CHUNK) as usize];
                    loop {
                        let n = f.read(&mut hbuf)?;
                        if n == 0 {
                            break;
                        }
                        bh.update(&hbuf[..n]);
                    }
                    Ok(f)
                })();
                let Ok(mut basef) = opened else {
                    extract_errors += 1;
                    crate::log_error!("pack", "ERR  delta base missing/unreadable: {rel}");
                    continue;
                };
                let base_hex = bh.finalize().to_hex().to_string();
                if meta.base_hash.as_deref() != Some(base_hex.as_str()) {
                    extract_errors += 1;
                    crate::log_error!(
                        "pack",
                        "ERR  delta base changed since chunk table: {rel} — rerun to repack"
                    );
                    continue;
                }
                let mut out = std::fs::File::create(&dst)?;
                let mut fh = blake3::Hasher::new();
                let mut buf = vec![0u8; 1 << 16];
                let mut ok = true;
                if let Some(recipe) = &meta.recipe {
                    'steps: for st in recipe {
                        match st.s.as_str() {
                            "base" => {
                                basef.seek(SeekFrom::Start(st.off))?;
                                let mut left = st.len as usize;
                                while left > 0 {
                                    let want = left.min(buf.len());
                                    let n = basef.read(&mut buf[..want])?;
                                    if n == 0 {
                                        ok = false;
                                        break 'steps;
                                    }
                                    fh.update(&buf[..n]);
                                    out.write_all(&buf[..n])?;
                                    left -= n;
                                }
                            }
                            "blob" => {
                                let start = st.off as usize;
                                let Some(end) = start.checked_add(st.len as usize) else {
                                    ok = false;
                                    break 'steps;
                                };
                                if end > blob.len() {
                                    ok = false;
                                    break 'steps;
                                }
                                fh.update(&blob[start..end]);
                                out.write_all(&blob[start..end])?;
                            }
                            _ => unreachable!("recipe sources were structurally validated"),
                        }
                    }
                }
                let final_hex = fh.finalize().to_hex().to_string();
                if !ok || final_hex != meta.hash {
                    extract_errors += 1;
                    crate::log_error!("pack", "ERR  delta reconstruction failed: {rel}");
                    let _ = std::fs::remove_file(&dst);
                } else if verbose {
                    println!(
                        "OK   delta reconstructed: {rel} (blob {} B of {} B)",
                        blob.len(),
                        meta.size
                    );
                }
            } else {
                let mut out = std::fs::File::create(&dst)?;
                let mut hasher = blake3::Hasher::new();
                let mut buf = [0u8; 1 << 16];
                let mut written = 0u64;
                loop {
                    let n = entry.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    out.write_all(&buf[..n])?;
                    written += n as u64;
                }
                let got = hasher.finalize().to_hex().to_string();
                if meta.hash == got && written == meta.size {
                    if verbose {
                        println!("OK   payload verified: {rel}");
                    }
                } else {
                    extract_errors += 1;
                    crate::log_error!(
                        "pack",
                        "ERR  payload hash mismatch: {rel} (manifest {} vs {got})",
                        meta.hash
                    );
                    let _ = std::fs::remove_file(&dst);
                }
            }
        }
    }
    let expected: std::collections::HashSet<_> =
        manifest.payload.iter().map(|p| p.rel.clone()).collect();
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

    // Execute through the normal apply transaction. Materialize the verified manifest metadata
    // into each operation so content, timestamps, and modes all publish while the root lease is
    // still held; package apply has no post-apply mutation lane.
    let mut ops = plan.ops.clone();
    for op in &mut ops {
        if matches!(op.action, Action::Copy | Action::Update) {
            if let Some(p) = by_rel.get(op.path.as_str()) {
                op.size = Some(p.size);
                op.hash = Some(p.hash.clone());
                op.mtime_ms = Some(p.mtime_ms);
                op.mode = p.mode;
            }
        }
    }
    let (done, skipped, errors) = crate::pipeline::apply::apply(
        &ops,
        staging.path(),
        &target_root,
        &crate::pipeline::apply::ApplyOptions {
            dry_run: false,
            verbose,
            verify: true,
            versioning,
            ..Default::default()
        },
    );

    Ok((done, skipped, errors))
}
