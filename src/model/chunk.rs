//! FastCDC content-defined chunking (v0.7 delta transfer).
//! Parameters are fixed at min/avg/max = 16K/64K/256K (v2020 gear) —— both ends must agree to land on the same cut points.
//! Only enabled for updated files ≥ DELTA_MIN_SIZE (below that, shipping the whole file is cheaper).

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DELTA_MIN_SIZE: u64 = 4 * 1024 * 1024;
const MIN: u32 = 16 * 1024;
const AVG: u32 = 64 * 1024;
const MAX: u32 = 256 * 1024;

#[derive(Serialize, Deserialize, Clone)]
pub struct ChunkInfo {
    pub off: u64,
    pub len: u32,
    pub hash: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileChunks {
    pub rel: String,
    pub size: u64,
    /// blake3 of the whole file
    pub hash: String,
    pub chunks: Vec<ChunkInfo>,
}

/// Delta reassembly step: s = "base" (take off..off+len from the receiver's existing file) | "blob" (take it from the delta blob).
///
/// It lives here rather than in `pack` because it describes how to reassemble a file out of chunks —
/// chunking semantics, unrelated to the tar container. It used to be defined in `pack.rs`, which made
/// the version store reach for `crate::transfer::pack::RecipeStep` for this one type; `pack` in turn calls
/// `crate::pipeline::apply`, and `apply` uses `version` — three modules wound into a cycle. Moving it onto this
/// zero-dependency leaf breaks that cycle.
#[derive(Serialize, Deserialize, Clone)]
pub struct RecipeStep {
    pub s: String,
    pub off: u64,
    pub len: u32,
}

pub fn chunk_bytes(data: &[u8]) -> Vec<ChunkInfo> {
    let mut out = Vec::new();
    for c in fastcdc::v2020::FastCDC::new(data, MIN, AVG, MAX) {
        let h = blake3::hash(&data[c.offset..c.offset + c.length]).to_hex().to_string();
        out.push(ChunkInfo { off: c.offset as u64, len: c.length as u32, hash: h });
    }
    out
}

pub fn chunk_file(root: &Path, rel: &str) -> std::io::Result<FileChunks> {
    let p = crate::foundation::path::join_native(root, rel);
    // Delta only kicks in for large files, and the memory ceiling here is the file itself; a GB-scale .mph is acceptable (one-shot, sequential read)
    let data = std::fs::read(&p)?;
    let hash = blake3::hash(&data).to_hex().to_string();
    Ok(FileChunks { rel: rel.to_string(), size: data.len() as u64, hash, chunks: chunk_bytes(&data) })
}
