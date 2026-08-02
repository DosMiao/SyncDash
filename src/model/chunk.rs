//! Wire vocabulary for FastCDC delta transfer.

use serde::{Deserialize, Serialize};

use crate::foundation::path::RootRelativePath;

pub const DELTA_MIN_SIZE: u64 = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
pub struct ChunkInfo {
    pub off: u64,
    pub len: u32,
    pub hash: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileChunks {
    pub rel: RootRelativePath,
    pub size: u64,
    /// blake3 of the whole file
    pub hash: String,
    pub chunks: Vec<ChunkInfo>,
}

/// Delta reassembly source: `base` reads the receiver's existing file and `blob` reads the
/// transferred delta blob. Keeping this vocabulary in `model` prevents a dependency cycle between
/// package apply and version storage.
#[derive(Serialize, Deserialize, Clone)]
pub struct RecipeStep {
    pub s: String,
    pub off: u64,
    pub len: u32,
}
