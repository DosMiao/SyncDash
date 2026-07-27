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
    /// 被过滤器排除的目录数（其下整棵子树都没进表）——排除必须可见，绝不静默
    #[serde(default)]
    pub excluded_dirs: u64,
    /// 被过滤器排除的文件数
    #[serde(default)]
    pub excluded_files: u64,
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
    /// unix 权限位（八进制 mode）。SMB 传不过去 exec 位，先记录，v0.4 打包模式恢复用。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<u32>,
    /// symlink 的指向（symlinks="direct" 时记录；比对按指向字符串相等）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub link: Option<String>,
    /// **仅 archive 表使用**：这条路径更早若干代的内容 hash，最新的在前（P1-3）。
    /// 用途：一侧的当前内容如果只是"停在某个历史版本上"，那它并没有并发修改，
    /// 不该报 both-changed。这是 archive 模型下对版本向量的廉价近似
    /// （syncthing 用 `PreviousBlocksHash` 达到同样目的，
    ///  `lib/protocol/bep_fileinfo.go:200-207`）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prev: Option<Vec<String>>,
}

/// archive 保留的历史代数。3 代足以覆盖"改了没刷存档"的常见情形，
/// 而每条只多几十字节。
pub const ARCHIVE_GENERATIONS: usize = 3;

impl Entry {
    /// 当前内容是否等于本条目**记录过的任一历史版本**
    pub fn matches_any_generation(&self, hash: &str) -> bool {
        if self.hash.as_deref() == Some(hash) {
            return true;
        }
        self.prev.as_ref().map(|v| v.iter().any(|h| h == hash)).unwrap_or(false)
    }
}

/// 生成新一代 archive：把旧 archive 里同路径的 hash 推进 `prev` 链。
/// `fresh` 是刚扫出来的快照（会被就地改写），`old` 是上一代 archive。
pub fn roll_generations(fresh: &mut [Entry], old: &[Entry]) {
    use std::collections::HashMap;
    let prior: HashMap<&str, &Entry> = old.iter().map(|e| (e.path.as_str(), e)).collect();
    for e in fresh.iter_mut() {
        let Some(o) = prior.get(e.path.as_str()) else { continue };
        // 内容没变就不必新增一代，避免历史被同一个 hash 灌满
        if o.hash.is_some() && o.hash == e.hash {
            e.prev = o.prev.clone();
            continue;
        }
        let mut chain = Vec::with_capacity(ARCHIVE_GENERATIONS);
        if let Some(h) = &o.hash {
            chain.push(h.clone());
        }
        if let Some(p) = &o.prev {
            chain.extend(p.iter().cloned());
        }
        chain.truncate(ARCHIVE_GENERATIONS);
        e.prev = if chain.is_empty() { None } else { Some(chain) };
    }
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
        Self::from_reader(std::io::BufReader::new(f))
    }

    /// 从任意流解析（ssh 远程扫描的 stdout 直接喂进来）
    pub fn from_reader(r: impl BufRead) -> std::io::Result<Snapshot> {
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
