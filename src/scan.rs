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

/// 初始化全局 rayon 池（哈希工作线程）：**降优先级**——standard 扫描的 BLAKE3
/// 刻意全核并行（让一次性成本尽快结束），但不该把整台机器压到卡顿；
/// below-normal 优先级下满核哈希会自动给前台程序让路，空闲机器上吞吐不变。
/// `SYNCDASH_SCAN_THREADS=N` 可再压线程数上限。CLI 与桌面启动时各调一次（幂等）。
pub fn init_worker_pool() {
    let mut b = rayon::ThreadPoolBuilder::new().thread_name(|i| format!("sd-hash-{i}"));
    if let Ok(n) = std::env::var("SYNCDASH_SCAN_THREADS") {
        if let Ok(n) = n.parse::<usize>() {
            if n >= 1 {
                b = b.num_threads(n.min(64));
            }
        }
    }
    // 已经有全局池（重复调用）时 build_global 返回 Err——静默即可
    let _ = b.start_handler(|_| lower_thread_priority()).build_global();
}

#[cfg(windows)]
fn lower_thread_priority() {
    // THREAD_PRIORITY_BELOW_NORMAL = -1（只降本线程，不动进程；UI 线程不受影响）
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(h: isize, p: i32) -> i32;
    }
    unsafe {
        SetThreadPriority(GetCurrentThread(), -1);
    }
}
#[cfg(target_os = "linux")]
fn lower_thread_priority() {
    // Linux 的 nice 是每线程的
    unsafe {
        libc::nice(3);
    }
}
#[cfg(all(unix, not(target_os = "linux")))]
fn lower_thread_priority() {
    // macOS 的 nice() 是进程级，动了会连 UI 一起降——不做
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

// ---------- mtime 校正表（P1-4）----------
//
// FAT/exFAT 只有 2 秒粒度，某些 SMB 服务端会自行截断/偏移写入的时间戳。
// 过去我们设完 mtime 就不管，靠比对时的 ±2s 容差硬扛——容差既可能漏判（真改动在 2 秒内）
// 也可能误判（偏移大于容差），而且 rigor = "quick" 时容差是**唯一**判据。
// syncthing 的 mtimeFS（`lib/fs/mtimefs.go:68`）写完立刻 stat 回来，
// 把 (ondisk, virtual) 存进库，之后一律对外报 virtual。这里做同样的事，
// 存储沿用已有的用户缓存目录，绝不污染被扫描的目录。

fn mtime_fix_file_for_root(root: &Path) -> PathBuf {
    let key = blake3::hash(root.to_string_lossy().to_lowercase().as_bytes());
    cache_dir().join(format!("{}.mtimefix.jsonl", &key.to_hex()[..16]))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MtimeFix {
    path: String,
    ondisk_ms: i64,
    intended_ms: i64,
}

/// path -> (ondisk, intended)
pub fn load_mtime_fixes(root: &Path) -> HashMap<String, (i64, i64)> {
    let mut map = HashMap::new();
    if let Ok(f) = std::fs::File::open(mtime_fix_file_for_root(root)) {
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            if let Ok(c) = serde_json::from_str::<MtimeFix>(&line) {
                map.insert(c.path, (c.ondisk_ms, c.intended_ms));
            }
        }
    }
    map
}

/// 记录若干条校正。同 path 以最新一条为准（读取时后写的覆盖先写的）。
pub fn record_mtime_fixes(root: &Path, fixes: &[(String, i64, i64)]) {
    if fixes.is_empty() {
        return;
    }
    let file = mtime_fix_file_for_root(root);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 合并后整体重写：避免无限追加
    let mut map = load_mtime_fixes(root);
    for (p, ondisk, intended) in fixes {
        map.insert(p.clone(), (*ondisk, *intended));
    }
    if let Ok(f) = std::fs::File::create(&file) {
        let mut w = std::io::BufWriter::new(f);
        for (path, (ondisk_ms, intended_ms)) in map {
            let rec = MtimeFix { path, ondisk_ms, intended_ms };
            let _ = writeln!(w, "{}", serde_json::to_string(&rec).unwrap());
        }
    }
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
    /// fast 严谨级：≥4MB 的文件不读全文，抽样摘要（size + 头/中/尾各 256KB 的 blake3，
    /// 值带 `~` 前缀与全量哈希严格隔离）。比 quick 多一层内容防线（截断、头尾/多数
    /// 原地修改、采样区 bitrot 全能抓），比 standard 少读约百倍字节；
    /// 云盘占位文件只水合三小段而不是整只下载。语义上不是逐字节相等证明。
    pub sampled: bool,
    /// symlinks="direct"：记录链接本身（指向字符串），否则忽略 symlink
    pub symlinks_direct: bool,
    /// FFS 语义的过滤器（见 filter.rs），默认排除已内置
    pub filter: crate::filter::PathFilter,
}

/// 抽样摘要的参数与实现（fast 严谨级）
pub const SAMPLE_MIN: u64 = 4 * 1024 * 1024;
const SAMPLE_CHUNK: usize = 256 * 1024;

/// fast 模式下这次扫描真正要读的字节（进度总量/速率按它算才诚实）
fn effective_read(size: u64, sampled: bool) -> u64 {
    if sampled && size >= SAMPLE_MIN {
        (3 * SAMPLE_CHUNK as u64).min(size)
    } else {
        size
    }
}

fn sampled_digest(path: &Path, size: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    let mut buf = vec![0u8; SAMPLE_CHUNK];
    for off in [0u64, size / 2, size.saturating_sub(SAMPLE_CHUNK as u64)] {
        f.seek(SeekFrom::Start(off))?;
        let mut read = 0usize;
        while read < SAMPLE_CHUNK {
            let n = f.read(&mut buf[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("~{}", hasher.finalize().to_hex()))
}

/// 扫描进度（P2-6）。syncthing 的 `FolderScanProgress` 同款信息量：
/// 阶段 + 已处理/总字节 + 速率，够前端画进度条与估算剩余时间。
#[derive(Clone, Copy, Debug)]
pub struct ScanProgress {
    /// "walk"（遍历元数据）| "hash"（并行哈希）
    pub phase: &'static str,
    pub files_total: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub mib_per_s: f64,
}

pub type ProgressFn<'a> = &'a (dyn Fn(ScanProgress) + Sync);

pub fn scan(root: &Path, opt: &ScanOptions) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, None, None)
}

pub fn scan_with_progress(
    root: &Path,
    opt: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, progress, None)
}

/// v0.9 M1 统一底座入口：取消/暂停/ProgressEvent 事件流（见 progress.rs）。
/// 旧的 ScanProgress 回调形态（P2-6）原样保留——两条通路共用同一个 scan_impl。
pub fn scan_ctx(
    root: &Path,
    opt: &ScanOptions,
    ctx: &crate::progress::RunCtx,
    phase: crate::progress::Phase,
) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, None, Some((ctx, phase)))
}

fn scan_impl(
    root: &Path,
    opt: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
    ctxp: Option<(&crate::progress::RunCtx, crate::progress::Phase)>,
) -> std::io::Result<Snapshot> {
    let pp = ctxp.map(|(ctx, phase)| {
        crate::progress::PhaseProgress::begin(ctx, phase, Some(root.to_string_lossy().into_owned()), 0, 0)
    });
    let side = match ctxp.map(|(_, ph)| ph) {
        Some(crate::progress::Phase::ScanTarget) => "target",
        _ => "source",
    };
    let started = now_ms();
    let t0 = std::time::Instant::now();
    let cache = if opt.hash { load_cache(root) } else { HashMap::new() };
    let mtime_fixes = load_mtime_fixes(root);
    let mut entries: Vec<Entry> = Vec::new();
    let mut hash_errors = 0u64;

    // Windows 长路径：遍历用 \\?\ 前缀的根，所有后代路径免疫 260 字符 MAX_PATH
    // （OneDrive 根前缀就 47 字符，深层课程目录实测撞线；撞线的目录整棵静默消失，
    // 在 mirror 里表现为"对面成片被删"。FFS 正是靠 \\?\ 活着的）。
    // 缓存键/快照头仍用原始 root，前缀只活在遍历与文件 I/O 里。
    #[cfg(windows)]
    let walk_root: std::path::PathBuf = {
        let s = root.to_string_lossy();
        if s.starts_with(r"\\") {
            root.to_path_buf() // 已是 \\?\ 或 UNC（\\server\share 需 \\?\UNC\ 形态，暂不转换）
        } else {
            std::path::PathBuf::from(format!(r"\\?\{}", s))
        }
    };
    #[cfg(not(windows))]
    let walk_root: std::path::PathBuf = root.to_path_buf();

    // walk 期的错误绝不再静默：计数 + 采样，收尾时如实喊出来
    // （被跳过的条目在比对里等价于"该侧不存在"——这是能生成灾难计划的隐身故障）
    let mut walk_errors = 0u64;
    let mut walk_err_samples: Vec<String> = Vec::new();

    // 排除必须可见：剪掉的目录/文件计数进快照头，界面明示"有多少没参与比对"。
    // （教训：默认排除静默吞掉 .git，界面还写"两侧一致 ✓"——绝不允许再发生）
    let excl_dirs = std::cell::Cell::new(0u64);
    let excl_files = std::cell::Cell::new(0u64);
    let walker = walkdir::WalkDir::new(&walk_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let rel = e
                .path()
                .strip_prefix(&walk_root)
                .unwrap_or(e.path())
                .to_string_lossy()
                .replace('\\', "/");
            if e.file_type().is_dir() {
                let (pass, child_might_match) = opt.filter.pass_dir(&rel);
                let keep = pass || child_might_match;
                if !keep {
                    excl_dirs.set(excl_dirs.get() + 1); // 整棵子树被剪
                }
                keep
            } else {
                let keep = opt.filter.pass_file(&rel);
                if !keep {
                    excl_files.set(excl_files.get() + 1);
                }
                keep
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
        if let Some(pp) = &pp {
            pp.checkpoint()?; // 取消/暂停协作点（每迭代一次 relaxed 原子读）
        }
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                walk_errors += 1;
                if walk_err_samples.len() < 5 {
                    walk_err_samples.push(format!(
                        "{}: {e}",
                        e.path().map(|p| p.display().to_string()).unwrap_or_default()
                    ));
                }
                continue;
            }
        };
        if item.depth() == 0 {
            continue;
        }
        let rel = item
            .path()
            .strip_prefix(&walk_root)
            .unwrap_or(item.path())
            .to_string_lossy()
            .replace('\\', "/");
        let md = match item.metadata() {
            Ok(m) => m,
            Err(e) => {
                walk_errors += 1;
                if walk_err_samples.len() < 5 {
                    walk_err_samples.push(format!("{rel}: {e}"));
                }
                continue;
            }
        };
        if item.file_type().is_dir() {
            if opt.filter.pass_dir(&rel).0 {
                entries.push(Entry { path: rel, kind: EntryKind::Dir, size: 0, mtime_ms: mtime_ms(&md), hash: None, file_id: None, mode: None, link: None, prev: None });
            }
        } else if item.file_type().is_symlink() {
            if opt.symlinks_direct {
                let target = std::fs::read_link(item.path())
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned());
                entries.push(Entry { path: rel, kind: EntryKind::Symlink, size: 0, mtime_ms: mtime_ms(&md), hash: None, file_id: None, mode: None, link: target, prev: None });
            }
        } else {
            let size = md.len();
            // P1-4：文件系统当初把我们要的 mtime 存成了别的值（FAT 2 秒粒度 / SMB 截断），
            // 这里换算回我们的本意，让比对不必依赖容差
            let raw_mt = mtime_ms(&md);
            let mt = match mtime_fixes.get(&rel) {
                Some((ondisk, intended)) if *ondisk == raw_mt => *intended,
                _ => raw_mt,
            };
            let mut hash = None;
            if opt.hash && !opt.force_rehash {
                if let Some((cs, cm, ch)) = cache.get(&rel) {
                    // 缓存值按模式隔离：`~` 前缀 = 抽样摘要，绝不能顶替全量哈希（反之亦然）
                    let want_sampled = opt.sampled && size >= SAMPLE_MIN;
                    if *cs == size && *cm == mt && ch.starts_with('~') == want_sampled {
                        hash = Some(ch.clone());
                    }
                }
            }
            pending.push(PendingFile { rel, abs: item.path().to_path_buf(), size, mt, hash, file_id: file_id(&md), mode: unix_mode(&md) });
            if let Some(pp) = &pp {
                pp.item_done(&pending.last().unwrap().rel);
            }
        }
    }

    if walk_errors > 0 {
        eprintln!(
            "warning: {walk_errors} entr(ies) under {} skipped by walk errors — they will look ABSENT on this side! samples: {}",
            root.display(),
            walk_err_samples.join(" | ")
        );
        if let Some(pp) = &pp {
            pp.error(
                "",
                "walk",
                side,
                &format!("{walk_errors} 个条目因遍历错误被跳过（该侧将视为不存在！）样本: {}", walk_err_samples.join(" | ")),
            );
        }
    }

    // walk 结束即知总量（P2-6 的洞察）：条数 = 文件数；字节 = 真要读盘哈希的那部分。
    // 条数计数换挡：walk 期计的是"发现了多少"，哈希期从零重计"处理完多少"
    // ——否则界面从哈希一开始就停在 N/N。
    if let Some(pp) = &pp {
        let bytes_to_hash: u64 = if opt.hash {
            pending.iter().filter(|p| p.hash.is_none()).map(|p| effective_read(p.size, opt.sampled)).sum()
        } else {
            0
        };
        pp.set_totals(pending.len() as u64, bytes_to_hash);
        pp.restart_items();
    }

    let hash_err_count = std::sync::atomic::AtomicU64::new(0);
    if opt.hash {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        const BIG_FILE: u64 = 32 * 1024 * 1024;
        // 只有缓存没命中的文件才真的要读，进度条按它们的字节数算才准
        let bytes_total: u64 = pending.iter().filter(|p| p.hash.is_none()).map(|p| p.size).sum();
        let files_total = pending.len() as u64;
        let bytes_done = AtomicU64::new(0);
        let hashing = AtomicBool::new(true);

        if let Some(cb) = progress {
            cb(ScanProgress { phase: "walk", files_total, bytes_total, bytes_done: 0, mib_per_s: 0.0 });
        }

        // 进度采样单独一个线程（syncthing 的 ProgressTicker 同款结构）：
        // 哈希线程只做一次 relaxed 累加，不承担任何回调开销
        std::thread::scope(|sc| {
            if let Some(cb) = progress {
                let (bd, hz, t_start) = (&bytes_done, &hashing, std::time::Instant::now());
                sc.spawn(move || {
                    while hz.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        let done = bd.load(Ordering::Relaxed);
                        let secs = t_start.elapsed().as_secs_f64().max(0.001);
                        cb(ScanProgress {
                            phase: "hash",
                            files_total,
                            bytes_total,
                            bytes_done: done,
                            mib_per_s: done as f64 / secs / (1024.0 * 1024.0),
                        });
                    }
                });
            }
            // ≥32MB 的文件不再一把 mmap 哈希到底：那会让"取消"要等在飞的大文件读完
            // （OneDrive 云占位文件还要先整只水合下载，可能一等几分钟）。
            // 分块读环每 8MiB 一个取消/暂停检查点；少了 blake3 的文件内多核，
            // 但外层文件级并行仍在，盘几乎总是瓶颈，吞吐差异进噪声。
            pending.par_iter_mut().for_each(|p| {
                if let Some(pp) = &pp {
                    if pp.checkpoint().is_err() {
                        return; // 取消：余下工单空转排空（rayon 标准提前退出法）
                    }
                }
                if p.hash.is_none() {
                    // fast 严谨级：大文件只读三个采样窗（云盘占位文件也只水合这三段）
                    if opt.sampled && p.size >= SAMPLE_MIN {
                        match sampled_digest(&p.abs, p.size) {
                            Ok(d) => p.hash = Some(d),
                            Err(e) => {
                                hash_err_count.fetch_add(1, Ordering::Relaxed);
                                if let Some(pp) = &pp {
                                    pp.error(&p.rel, "hash", side, &e.to_string());
                                }
                            }
                        }
                        let eff = effective_read(p.size, true);
                        bytes_done.fetch_add(eff, Ordering::Relaxed);
                        if let Some(pp) = &pp {
                            pp.add_bytes(eff, &p.rel);
                            pp.item_done(&p.rel);
                        }
                        return;
                    }
                    let mut hasher = blake3::Hasher::new();
                    let res = if p.size >= BIG_FILE {
                        (|| -> std::io::Result<()> {
                            use std::io::Read;
                            let mut f = std::fs::File::open(&p.abs)?;
                            let mut buf = vec![0u8; 8 * 1024 * 1024];
                            loop {
                                if let Some(pp) = &pp {
                                    pp.checkpoint()?;
                                }
                                let n = f.read(&mut buf)?;
                                if n == 0 {
                                    break;
                                }
                                hasher.update(&buf[..n]);
                            }
                            Ok(())
                        })()
                    } else {
                        hasher.update_mmap(&p.abs).map(|_| ())
                    };
                    match res {
                        Ok(_) => p.hash = Some(hasher.finalize().to_hex().to_string()),
                        Err(e) if crate::progress::is_cancelled(&e) => return, // 取消不是哈希错误
                        Err(e) => {
                            hash_err_count.fetch_add(1, Ordering::Relaxed);
                            if let Some(pp) = &pp {
                                pp.error(&p.rel, "hash", side, &e.to_string());
                            }
                        }
                    }
                    bytes_done.fetch_add(p.size, Ordering::Relaxed);
                    if let Some(pp) = &pp {
                        pp.add_bytes(p.size, &p.rel);
                    }
                }
                if let Some(pp) = &pp {
                    pp.item_done(&p.rel); // 哈希期的条数 = 处理完的文件数（缓存命中即刻+1）
                }
            });
            hashing.store(false, Ordering::Relaxed);
        });
        if let Some(pp) = &pp {
            pp.checkpoint()?; // 取消发生在哈希期时，从这里如实返回 Interrupted
        }

        if let Some(cb) = progress {
            cb(ScanProgress {
                phase: "hash",
                files_total,
                bytes_total,
                bytes_done: bytes_total,
                mib_per_s: 0.0,
            });
        }
    }
    for p in pending {
        entries.push(Entry { path: p.rel, kind: EntryKind::File, size: p.size, mtime_ms: p.mt, hash: p.hash, file_id: p.file_id, mode: p.mode, link: None, prev: None });
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
            excluded_dirs: excl_dirs.get(),
            excluded_files: excl_files.get(),
        },
        entries,
    })
}

#[cfg(test)]
mod ctx_tests {
    use super::*;
    use crate::progress::{is_cancelled, Phase, ProgressEvent, RunCtl, RunCtx};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    fn mk_tree(tag: &str, n: usize) -> PathBuf {
        let root = std::env::temp_dir().join(format!("syncdash-scanctx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for i in 0..n {
            std::fs::write(root.join("sub").join(format!("f{i}.dat")), vec![i as u8; 100]).unwrap();
        }
        root
    }

    #[test]
    fn sampled_digest_catches_edge_edits_only() {
        let d = std::env::temp_dir().join(format!("sd-sample-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("s.bin");
        let mut data = vec![5u8; 8 * 1024 * 1024];
        std::fs::write(&f, &data).unwrap();
        let a = sampled_digest(&f, data.len() as u64).unwrap();
        assert!(a.starts_with('~'), "sampled digests carry the ~ marker");
        // 中点在采样窗内 → 摘要必须变
        data[4 * 1024 * 1024 + 10] = 9;
        std::fs::write(&f, &data).unwrap();
        let b = sampled_digest(&f, data.len() as u64).unwrap();
        assert_ne!(a, b, "an edit inside a sample window must change the digest");
        // 1MB 处在三个采样窗之外 → 摘要不变。这是 fast 的安全边界，如实断言
        data[1024 * 1024] = 7;
        std::fs::write(&f, &data).unwrap();
        let c = sampled_digest(&f, data.len() as u64).unwrap();
        assert_eq!(b, c, "fast mode by design does not see edits outside sample windows");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn opts() -> ScanOptions {
        ScanOptions {
            hash: true,
            sampled: false,
            force_rehash: true, // 测试不吃缓存
            symlinks_direct: false,
            filter: crate::filter::PathFilter::build(&[], &[]),
        }
    }

    #[test]
    fn scan_ctx_reports_exact_totals() {
        let root = mk_tree("totals", 20);
        let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let ctx = RunCtx::new(RunCtl::new(), Arc::new(move |ev| s2.lock().unwrap().push(ev)));
        let snap = scan_ctx(&root, &opts(), &ctx, Phase::ScanSource).unwrap();
        assert_eq!(snap.entries.iter().filter(|e| e.kind == EntryKind::File).count(), 20);
        let evs = store.lock().unwrap();
        let totals = evs.iter().find_map(|e| match e {
            ProgressEvent::Totals { items_total, bytes_total, .. } => Some((*items_total, *bytes_total)),
            _ => None,
        });
        assert_eq!(totals, Some((20, 2000)), "walk 结束必须给出精确总量");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_ctx_cancels_midway() {
        let root = mk_tree("cancel", 50);
        let ctl = RunCtl::new();
        let ctl2 = ctl.clone();
        let sink = move |ev: ProgressEvent| {
            if matches!(ev, ProgressEvent::Progress { .. }) {
                ctl2.cancel.store(true, Ordering::SeqCst); // 第一个进度事件就喊停
            }
        };
        let ctx = RunCtx::new(ctl, Arc::new(sink));
        match scan_ctx(&root, &opts(), &ctx, Phase::ScanSource) {
            Err(e) => assert!(is_cancelled(&e)),
            Ok(_) => panic!("expected cancellation"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
