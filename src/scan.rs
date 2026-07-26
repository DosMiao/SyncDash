//! scan：走目录树，产出快照表（要求 1 的"通用命令"）。
//! - 排除已知垃圾/可重建目录（与 CodeSync 的 FFS 排除口径一致的子集）
//! - blake3 内容 hash，带缓存：(path,size,mtime) 未变则复用上次 hash，避免每次重算几十 GB
//! - 缓存放在本机用户缓存目录，绝不污染被扫描的目录

use crate::table::{now_ms, os_name, Entry, EntryKind, Header, Snapshot, SCHEMA};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn file_id(md: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("{}:{}", md.dev(), md.ino()))
}
#[cfg(not(unix))]
fn file_id(_md: &std::fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
fn unix_mode(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(md.mode() & 0o7777)
}
#[cfg(not(unix))]
fn unix_mode(_md: &std::fs::Metadata) -> Option<u32> {
    None
}

fn mtime_ms(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- hash 缓存 ----------

fn cache_dir() -> PathBuf {
    if let Ok(l) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(l).join("syncdash").join("hashcache")
    } else if let Ok(h) = std::env::var("HOME") {
        PathBuf::from(h).join(".cache").join("syncdash").join("hashcache")
    } else {
        PathBuf::from(".syncdash-cache")
    }
}

fn cache_file_for_root(root: &Path) -> PathBuf {
    let key = blake3::hash(root.to_string_lossy().to_lowercase().as_bytes());
    cache_dir().join(format!("{}.jsonl", &key.to_hex()[..16]))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheLine {
    path: String,
    size: u64,
    mtime_ms: i64,
    hash: String,
}

fn load_cache(root: &Path) -> HashMap<String, (u64, i64, String)> {
    let mut map = HashMap::new();
    if let Ok(f) = std::fs::File::open(cache_file_for_root(root)) {
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            if let Ok(c) = serde_json::from_str::<CacheLine>(&line) {
                map.insert(c.path, (c.size, c.mtime_ms, c.hash));
            }
        }
    }
    map
}

fn save_cache(root: &Path, entries: &[Entry]) {
    let file = cache_file_for_root(root);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(f) = std::fs::File::create(&file) {
        let mut w = std::io::BufWriter::new(f);
        for e in entries {
            if let Some(h) = &e.hash {
                let c = CacheLine { path: e.path.clone(), size: e.size, mtime_ms: e.mtime_ms, hash: h.clone() };
                let _ = writeln!(w, "{}", serde_json::to_string(&c).unwrap());
            }
        }
    }
}

// ---------- 扫描 ----------

pub struct ScanOptions {
    pub hash: bool,
    /// paranoid 严谨级：无视 (size,mtime) 缓存，全部重新 hash
    pub force_rehash: bool,
    /// FFS 语义的过滤器（见 filter.rs），默认排除已内置
    pub filter: crate::filter::PathFilter,
}

pub fn scan(root: &Path, opt: &ScanOptions) -> std::io::Result<Snapshot> {
    let started = now_ms();
    let t0 = std::time::Instant::now();
    let cache = if opt.hash { load_cache(root) } else { HashMap::new() };
    let mut entries: Vec<Entry> = Vec::new();
    let mut hash_errors = 0u64;

    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap_or(e.path())
                .to_string_lossy()
                .replace('\\', "/");
            if e.file_type().is_dir() {
                let (pass, child_might_match) = opt.filter.pass_dir(&rel);
                pass || child_might_match
            } else {
                opt.filter.pass_file(&rel)
            }
        });

    // 阶段 1：串行遍历收集（元数据快）；阶段 2：rayon 并行哈希（FFS parallel_scan 的启示：
    // 多小文件时瓶颈是串行 I/O）。大文件内部再用 mmap_rayon 分块，小文件用普通 mmap 免过订阅。
    struct PendingFile {
        rel: String,
        abs: std::path::PathBuf,
        size: u64,
        mt: i64,
        hash: Option<String>, // 缓存命中
        file_id: Option<String>,
        mode: Option<u32>,
    }
    let mut pending: Vec<PendingFile> = Vec::new();

    for item in walker {
        let item = match item {
            Ok(i) => i,
            Err(_) => continue, // 无权限等：跳过但不中断
        };
        if item.depth() == 0 {
            continue;
        }
        let rel = item
            .path()
            .strip_prefix(root)
            .unwrap_or(item.path())
            .to_string_lossy()
            .replace('\\', "/");
        let md = match item.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if item.file_type().is_dir() {
            if opt.filter.pass_dir(&rel).0 {
                entries.push(Entry { path: rel, kind: EntryKind::Dir, size: 0, mtime_ms: mtime_ms(&md), hash: None, file_id: None, mode: None });
            }
        } else if item.file_type().is_symlink() {
            entries.push(Entry { path: rel, kind: EntryKind::Symlink, size: 0, mtime_ms: mtime_ms(&md), hash: None, file_id: None, mode: None });
        } else {
            let size = md.len();
            let mt = mtime_ms(&md);
            let mut hash = None;
            if opt.hash && !opt.force_rehash {
                if let Some((cs, cm, ch)) = cache.get(&rel) {
                    if *cs == size && *cm == mt {
                        hash = Some(ch.clone());
                    }
                }
            }
            pending.push(PendingFile { rel, abs: item.path().to_path_buf(), size, mt, hash, file_id: file_id(&md), mode: unix_mode(&md) });
        }
    }

    let hash_err_count = std::sync::atomic::AtomicU64::new(0);
    if opt.hash {
        use rayon::prelude::*;
        const BIG_FILE: u64 = 32 * 1024 * 1024;
        pending.par_iter_mut().for_each(|p| {
            if p.hash.is_none() {
                let mut hasher = blake3::Hasher::new();
                let res = if p.size >= BIG_FILE {
                    hasher.update_mmap_rayon(&p.abs).map(|_| ())
                } else {
                    hasher.update_mmap(&p.abs).map(|_| ())
                };
                match res {
                    Ok(_) => p.hash = Some(hasher.finalize().to_hex().to_string()),
                    Err(_) => {
                        hash_err_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        });
    }
    for p in pending {
        entries.push(Entry { path: p.rel, kind: EntryKind::File, size: p.size, mtime_ms: p.mt, hash: p.hash, file_id: p.file_id, mode: p.mode });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    if opt.hash {
        save_cache(root, &entries);
    }
    hash_errors += hash_err_count.load(std::sync::atomic::Ordering::Relaxed);
    if hash_errors > 0 {
        eprintln!("warning: {hash_errors} file(s) could not be hashed (in use / unreadable)");
    }

    Ok(Snapshot {
        header: Header {
            schema: SCHEMA,
            kind: "snapshot".into(),
            root: root.to_string_lossy().into_owned(),
            host: crate::table::host_name(),
            os: os_name(),
            scanned_at_ms: started,
            duration_ms: t0.elapsed().as_millis() as u64,
            entry_count: entries.len() as u64,
            hashed: opt.hash,
        },
        entries,
    })
}
