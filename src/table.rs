//! 表格式：JSONL —— 第一行是 Header，之后每行一个 Entry。
//! 选 JSONL 的原因：可流式产出/解析（ssh 管道直接传）、可增量追加、坏一行不坏整表。

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

pub const SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Header {
    pub schema: u32,
    pub kind: String, // "snapshot" | "plan" | "archive"
    pub root: String,
    pub host: String,
    pub os: String,
    pub scanned_at_ms: u64,
    pub duration_ms: u64,
    pub entry_count: u64,
    pub hashed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    /// 相对 root 的路径，统一 '/' 分隔（跨平台可比）
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// unix: dev:inode；windows 暂空。仅用于同机 move 佐证，跨机靠 hash。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

pub struct Snapshot {
    pub header: Header,
    pub entries: Vec<Entry>,
}

impl Snapshot {
    pub fn write_to(&self, w: &mut dyn Write) -> std::io::Result<()> {
        writeln!(w, "{}", serde_json::to_string(&self.header)?)?;
        for e in &self.entries {
            writeln!(w, "{}", serde_json::to_string(e)?)?;
        }
        Ok(())
    }

    pub fn load(path: &Path) -> std::io::Result<Snapshot> {
        let f = std::fs::File::open(path)?;
        let r = std::io::BufReader::new(f);
        let mut lines = r.lines();
        let head_line = lines
            .next()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "empty table"))??;
        let header: Header = serde_json::from_str(&head_line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad header: {e}")))?;
        let mut entries = Vec::new();
        for line in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let e: Entry = serde_json::from_str(&line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad entry: {e}")))?;
            entries.push(e);
        }
        Ok(Snapshot { header, entries })
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn os_name() -> String {
    std::env::consts::OS.to_string()
}

pub fn host_name() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into())
}
