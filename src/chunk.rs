//! FastCDC 内容定义分块（v0.7 增量传输）。
//! 参数固定 min/avg/max = 16K/64K/256K（v2020 gear）——两端必须一致才能命中相同切点。
//! 只对 ≥ DELTA_MIN_SIZE 的更新文件启用（小文件整传更划算）。

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
    /// 整文件 blake3
    pub hash: String,
    pub chunks: Vec<ChunkInfo>,
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
    let native = if cfg!(windows) { rel.replace('/', "\\") } else { rel.to_string() };
    let p = root.join(native);
    // 增量只对大文件启用，但读入内存的上限就是文件本身；GB 级 .mph 也可接受（一次性、顺序读）
    let data = std::fs::read(&p)?;
    let hash = blake3::hash(&data).to_hex().to_string();
    Ok(FileChunks { rel: rel.to_string(), size: data.len() as u64, hash, chunks: chunk_bytes(&data) })
}
