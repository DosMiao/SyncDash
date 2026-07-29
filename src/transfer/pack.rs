//! v0.4 remote packing (requirement 4):
//!   pack       —— bundle the target-side ops of a plan into one tar package:
//!                 plan.jsonl (the op manifest) + payload/<rel> (files to write) + manifest.json (the wrap-up)
//!                 the manifest carries: the plan's blake3, each payload file's blake3/size/mtime/unix mode,
//!                 and a combined hash (concatenate the per-file hashes in order, then blake3)
//!   apply-pack —— run on the far end: verify the plan hash → extract file by file into staging, verifying each hash →
//!                 reuse apply::apply (lock, trash directory, post-copy verification all included) → restore unix mode
//! dry-run by default; path safety: absolute paths and `..` components are rejected.

use crate::model::plan::{Action, Op, Plan, PlanHeader, Side};
// The real path-safety check and the `rel → native separator` conversion both live in `foundation::path`.
// This file used to carry its own copy of each (`rel_is_safe`/`to_native`), verbatim duplicates of those.
use crate::model::chunk::RecipeStep;
use crate::foundation::path::{is_safe_rel, to_native};
use crate::foundation::time::now_ms;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const PACK_VERSION: u32 = 2;

/// Read granularity for hashing a delta base off the target root.
const READ_CHUNK: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
pub struct PayloadEntry {
    pub rel: String,
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

struct HashingReader<R: Read> {
    inner: R,
    hasher: blake3::Hasher,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
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

/// Pack the target-side ops of a plan. Payload is read from source_root.
/// remote_chunks: FastCDC chunk tables for the large files the remote already has (when present, Updates ≥4MB go delta).
pub fn pack(plan: &Plan, source_root: &Path, out: &Path, remote_chunks: Option<&std::collections::HashMap<String, crate::model::chunk::FileChunks>>) -> std::io::Result<PackSummary> {
    let target_ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| o.side == Side::Target && !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    let skipped_source_side = plan.ops.iter().filter(|o| o.side == Side::Source).count();
    if skipped_source_side > 0 {
        crate::log_info!("pack", "note: {skipped_source_side} source-side op(s) not packed (they run locally, not on the remote)");
    }
    for op in &target_ops {
        if !is_safe_rel(&op.path) || op.from.as_deref().map(|f| !is_safe_rel(f)).unwrap_or(false) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unsafe path in plan: {}", op.path)));
        }
    }

    // Sub-plan: same header, target-side ops only
    let sub = Plan { header: PlanHeader { op_count: target_ops.len() as u64, ..plan.header.clone() }, ops: target_ops.clone() };
    let mut plan_bytes: Vec<u8> = Vec::new();
    sub.write_to(&mut plan_bytes)?;
    let plan_hash = blake3::hash(&plan_bytes).to_hex().to_string();

    let f = std::fs::File::create(out)?;
    let mut tarb = tar::Builder::new(std::io::BufWriter::new(f));
    tarb.append_data(&mut tar_header(plan_bytes.len() as u64), "plan.jsonl", &plan_bytes[..])?;

    // payload: the content behind Copy/Update, deduplicated; large files that hit remote_chunks go through FastCDC delta
    let mut seen = std::collections::HashSet::new();
    let mut payload: Vec<PayloadEntry> = Vec::new();
    let mut bytes_total = 0u64;
    let mut delta_saved = 0u64;
    for op in &target_ops {
        if !matches!(op.action, Action::Copy | Action::Update) || !seen.insert(op.path.clone()) {
            continue;
        }
        if op.link.is_some() {
            continue; // a symlink has no payload
        }
        let src = source_root.join(to_native(&op.path));
        let md = std::fs::metadata(&src)?;
        let size = md.len();
        let mtime_ms = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            Some(md.mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let mode: Option<u32> = None;

        let base = remote_chunks.and_then(|m| m.get(&op.path)).filter(|_| size >= crate::model::chunk::DELTA_MIN_SIZE);
        if let Some(base) = base {
            // Delta: chunk the new local file, align it against the remote chunk table by hash, pack only the missing chunks
            let data = std::fs::read(&src)?;
            let hash = blake3::hash(&data).to_hex().to_string();
            let local_chunks = crate::model::chunk::chunk_bytes(&data);
            let mut base_by_hash: std::collections::HashMap<&str, (u64, u32)> = std::collections::HashMap::new();
            for c in &base.chunks {
                base_by_hash.entry(c.hash.as_str()).or_insert((c.off, c.len));
            }
            let mut blob: Vec<u8> = Vec::new();
            let mut recipe: Vec<RecipeStep> = Vec::new();
            for c in &local_chunks {
                if let Some(&(boff, blen)) = base_by_hash.get(c.hash.as_str()) {
                    recipe.push(RecipeStep { s: "base".into(), off: boff, len: blen });
                } else {
                    let off = blob.len() as u64;
                    blob.extend_from_slice(&data[c.off as usize..(c.off + c.len as u64) as usize]);
                    recipe.push(RecipeStep { s: "blob".into(), off, len: c.len });
                }
            }
            let blob_hash = blake3::hash(&blob).to_hex().to_string();
            tarb.append_data(&mut tar_header(blob.len() as u64), format!("payload/{}", op.path), &blob[..])?;
            bytes_total += blob.len() as u64;
            delta_saved += size.saturating_sub(blob.len() as u64);
            payload.push(PayloadEntry {
                rel: op.path.clone(),
                size,
                mtime_ms,
                hash,
                mode,
                kind: "delta".into(),
                base_hash: Some(base.hash.clone()),
                blob_hash: Some(blob_hash),
                recipe: Some(recipe),
            });
        } else {
            let file = std::fs::File::open(&src)?;
            let mut hr = HashingReader { inner: std::io::BufReader::new(file), hasher: blake3::Hasher::new() };
            tarb.append_data(&mut tar_header(size), format!("payload/{}", op.path), &mut hr)?;
            let hash = hr.hasher.finalize().to_hex().to_string();
            bytes_total += size;
            payload.push(PayloadEntry { rel: op.path.clone(), size, mtime_ms, hash, mode, kind: "whole".into(), base_hash: None, blob_hash: None, recipe: None });
        }
    }

    let mut comb = blake3::Hasher::new();
    for p in &payload {
        comb.update(p.hash.as_bytes());
    }
    let manifest = Manifest {
        schema: crate::model::table::SCHEMA,
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
    tarb.append_data(&mut tar_header(mani_bytes.len() as u64), "manifest.json", &mani_bytes[..])?;
    tarb.into_inner()?.flush()?;

    Ok(PackSummary { ops: manifest.op_count, files: manifest.payload.len() as u64, bytes: bytes_total, delta_saved })
}

/// Execute a package on the far end. dry_run=true only verifies and lists. Returns (done, skipped, errors).
pub fn apply_pack(
    pkg: &Path,
    target_root_override: Option<&Path>,
    do_apply: bool,
    verbose: bool,
    versioning: bool,
) -> std::io::Result<(u64, u64, u64)> {
    // pass 1: read plan.jsonl and manifest.json
    let mut plan_bytes: Option<Vec<u8>> = None;
    let mut manifest: Option<Manifest> = None;
    {
        let f = std::fs::File::open(pkg)?;
        let mut ar = tar::Archive::new(std::io::BufReader::new(f));
        for entry in ar.entries()? {
            let mut entry = entry?;
            let name = entry.path()?.to_string_lossy().into_owned();
            if name == "plan.jsonl" {
                let mut v = Vec::new();
                entry.read_to_end(&mut v)?;
                plan_bytes = Some(v);
            } else if name == "manifest.json" {
                let mut v = Vec::new();
                entry.read_to_end(&mut v)?;
                manifest = Some(serde_json::from_slice(&v).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad manifest: {e}"))
                })?);
            }
        }
    }
    let plan_bytes = plan_bytes.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "package has no plan.jsonl"))?;
    let manifest = manifest.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "package has no manifest.json"))?;

    // Plan integrity + version
    if manifest.pack_version > PACK_VERSION {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("package version {} newer than this binary supports ({PACK_VERSION}) — rebuild the remote", manifest.pack_version)));
    }
    let got = blake3::hash(&plan_bytes).to_hex().to_string();
    if got != manifest.plan_blake3 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("plan hash mismatch: manifest {} vs actual {got}", manifest.plan_blake3)));
    }

    // Parse the plan straight from the bytes we just verified. It used to go out to a temp file
    // named only by pid and come back through Plan::load, which meant two apply_pack calls in one
    // process shared a path — one deleting the file the other was still reading.
    let plan = Plan::from_reader(std::io::BufReader::new(&plan_bytes[..]))?;

    for op in &plan.ops {
        if !is_safe_rel(&op.path) || op.from.as_deref().map(|f| !is_safe_rel(f)).unwrap_or(false) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unsafe path in package plan: {}", op.path)));
        }
    }

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
        println!("dry-run OK: plan hash verified; rerun with --apply");
        return Ok((0, plan.ops.len() as u64, 0));
    }

    if !target_root.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("target root not accessible: {}", target_root.display())));
    }

    // pass 2: extract payload into staging, verifying each file's hash
    let staging = std::env::temp_dir().join(format!("syncdash-staging-{}-{}", std::process::id(), now_ms()));
    std::fs::create_dir_all(&staging)?;
    let by_rel: std::collections::HashMap<&str, &PayloadEntry> = manifest.payload.iter().map(|p| (p.rel.as_str(), p)).collect();
    let mut extract_errors = 0u64;
    {
        let f = std::fs::File::open(pkg)?;
        let mut ar = tar::Archive::new(std::io::BufReader::new(f));
        for entry in ar.entries()? {
            let mut entry = entry?;
            let name = entry.path()?.to_string_lossy().into_owned();
            let Some(rel) = name.strip_prefix("payload/") else { continue };
            let rel = rel.to_string();
            if !is_safe_rel(&rel) {
                extract_errors += 1;
                crate::log_error!("pack", "ERR  unsafe payload path skipped: {rel}");
                continue;
            }
            let Some(&meta) = by_rel.get(rel.as_str()) else {
                extract_errors += 1;
                crate::log_error!("pack", "ERR  payload not in manifest: {rel}");
                continue;
            };
            let dst = staging.join(to_native(&rel));
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
                use std::io::{Seek, SeekFrom};
                let base_path = target_root.join(to_native(&rel));
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
                    crate::log_error!("pack", "ERR  delta base changed since chunk table: {rel} — rerun to repack");
                    continue;
                }
                let mut out = std::fs::File::create(&dst)?;
                let mut fh = blake3::Hasher::new();
                let mut buf = vec![0u8; 1 << 16];
                let mut ok = true;
                if let Some(recipe) = &meta.recipe {
                    'steps: for st in recipe {
                        if st.s == "base" {
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
                        } else {
                            let s = st.off as usize;
                            let e = s + st.len as usize;
                            if e > blob.len() {
                                ok = false;
                                break 'steps;
                            }
                            fh.update(&blob[s..e]);
                            out.write_all(&blob[s..e])?;
                        }
                    }
                }
                let final_hex = fh.finalize().to_hex().to_string();
                if !ok || final_hex != meta.hash {
                    extract_errors += 1;
                    crate::log_error!("pack", "ERR  delta reconstruction failed: {rel}");
                    let _ = std::fs::remove_file(&dst);
                } else if verbose {
                    println!("OK   delta reconstructed: {rel} (blob {} B of {} B)", blob.len(), meta.size);
                }
            } else {
                let mut out = std::fs::File::create(&dst)?;
                let mut hasher = blake3::Hasher::new();
                let mut buf = [0u8; 1 << 16];
                loop {
                    let n = entry.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    out.write_all(&buf[..n])?;
                }
                let got = hasher.finalize().to_hex().to_string();
                if meta.hash == got {
                    if verbose {
                        println!("OK   payload verified: {rel}");
                    }
                } else {
                    extract_errors += 1;
                    crate::log_error!("pack", "ERR  payload hash mismatch: {rel} (manifest {} vs {got})", meta.hash);
                    let _ = std::fs::remove_file(&dst);
                }
            }
        }
    }
    if extract_errors > 0 {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{extract_errors} payload file(s) failed verification — nothing applied")));
    }

    // execute: reuse apply (lock, trash directory, post-copy verification)
    // Align each op's hash/mtime with the manifest (the state at pack time) so apply's verify re-checks against it
    let mut ops = plan.ops.clone();
    for op in &mut ops {
        if matches!(op.action, Action::Copy | Action::Update) {
            if let Some(p) = by_rel.get(op.path.as_str()) {
                op.hash = Some(p.hash.clone());
                op.mtime_ms = Some(p.mtime_ms);
            }
        }
    }
    let (done, skipped, errors) = crate::pipeline::apply::apply(
        &ops,
        &staging,
        &target_root,
        &crate::pipeline::apply::ApplyOptions { dry_run: false, verbose, verify: true, versioning, ..Default::default() },
    );

    // unix: restore the exec and other permission bits (the one attribute lost along the SMB/pack route)
    #[cfg(unix)]
    if errors == 0 {
        use std::os::unix::fs::PermissionsExt;
        for op in &ops {
            if matches!(op.action, Action::Copy | Action::Update) {
                if let Some(p) = by_rel.get(op.path.as_str()) {
                    if let Some(mode) = p.mode {
                        let _ = std::fs::set_permissions(target_root.join(to_native(&op.path)), std::fs::Permissions::from_mode(mode));
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_dir_all(&staging);
    Ok((done, skipped, errors))
}
