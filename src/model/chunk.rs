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

#[cfg(test)]
mod tests {
    use super::*;

    /// The delta path only saves anything if both sides chunk identical bytes identically —
    /// the boundaries are content-defined, so this is a property of the data, not of the run.
    #[test]
    fn chunking_is_deterministic_for_the_same_bytes() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let a = chunk_bytes(&data);
        let b = chunk_bytes(&data);
        assert!(!a.is_empty(), "a 200KB buffer must produce chunks");
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.hash, y.hash);
            assert_eq!((x.off, x.len), (y.off, y.len));
        }
    }

    /// Chunks must tile the input exactly: no gap (data would be lost) and no overlap
    /// (data would be duplicated) when a file is reassembled from a recipe.
    #[test]
    fn chunks_tile_the_input_without_gap_or_overlap() {
        let data: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
        let cs = chunk_bytes(&data);
        let mut at = 0u64;
        for c in &cs {
            assert_eq!(c.off, at, "chunk starts where the previous one ended");
            at += c.len as u64;
        }
        assert_eq!(at, data.len() as u64, "the chunks together must cover the whole input");
    }

    /// An edit in the middle must leave most boundaries alone — that is the entire reason
    /// this is content-defined chunking rather than fixed blocks.
    #[test]
    fn an_edit_disturbs_only_nearby_boundaries() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8).collect();
        let mut edited = data.clone();
        for b in edited.iter_mut().skip(150_000).take(64) {
            *b ^= 0xFF;
        }
        let a = chunk_bytes(&data);
        let b = chunk_bytes(&edited);
        let shared = a.iter().filter(|c| b.iter().any(|d| d.hash == c.hash)).count();
        assert!(
            shared * 2 >= a.len(),
            "a 64-byte edit should leave over half of {} chunks reusable, kept {shared}",
            a.len()
        );
    }
}
