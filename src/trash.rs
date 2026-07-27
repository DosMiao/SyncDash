//! 本机回收目录的保留策略与找回（P2-2）。
//!
//! `apply` 每跑一次就在 `<trash_root>/<毫秒时间戳>/` 下留一份"被删/被覆盖"的原件。
//! 过去这些目录**从不清理**，长期使用会吃满盘，而且几百个时间戳目录里找一个文件靠运气。
//!
//! 这里补三件事（语义参照 syncthing `lib/versioner/`）：
//!   - `find`  —— 跨所有时间戳目录找同一个路径的历史版本（回收站的正确用法）
//!   - `restore` —— 取回某一版（默认 dry-run）
//!   - `prune` —— 按保留天数 / 总体积上限清理；可选 **staggered 稀释**
//!     （`lib/versioner/staggered.go:47-53` 的区间表：第一小时每 30 秒留一份、
//!      当天每小时一份、30 天内每天一份、之后每周一份）
//!
//! 注意：`versioning = true` 的任务走的是各 root 内的 `.version_syncDash/`（见 version.rs），
//! 与这里的本机 trash 是两条独立的路径；本模块只管本机 trash。

use std::path::{Path, PathBuf};

pub fn trash_root() -> PathBuf {
    let base = if let Ok(l) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(l).join("syncdash")
    } else if let Ok(h) = std::env::var("HOME") {
        PathBuf::from(h).join(".cache").join("syncdash")
    } else {
        PathBuf::from(".syncdash")
    };
    base.join("trash")
}

#[derive(Clone, Debug)]
pub struct Run {
    /// 目录名（毫秒时间戳）
    pub id: String,
    pub at_ms: u64,
    pub dir: PathBuf,
    pub files: u64,
    pub bytes: u64,
}

fn dir_stats(d: &Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    for e in walkdir::WalkDir::new(d).follow_links(false).into_iter().flatten() {
        if e.file_type().is_file() {
            files += 1;
            bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    (files, bytes)
}

/// 列出全部回收批次，按时间从旧到新
pub fn list_runs() -> Vec<Run> {
    let root = trash_root();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return out;
    };
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let id = e.file_name().to_string_lossy().into_owned();
        let Ok(at_ms) = id.parse::<u64>() else { continue };
        let (files, bytes) = dir_stats(&e.path());
        out.push(Run { id, at_ms, dir: e.path(), files, bytes });
    }
    out.sort_by_key(|r| r.at_ms);
    out
}

#[derive(Clone, Debug)]
pub struct Found {
    pub run_id: String,
    pub at_ms: u64,
    /// 回收目录里的绝对路径
    pub stored: PathBuf,
    /// 相对 root 的原路径（'/' 分隔）
    pub rel: String,
    pub size: u64,
}

/// 跨全部批次查找路径含 `needle`（大小写不敏感子串）的文件。
/// 结果按时间从新到旧——找回时你几乎总是想要最近那一版。
pub fn find(needle: &str) -> Vec<Found> {
    let needle = needle.to_lowercase();
    let mut out = Vec::new();
    for run in list_runs() {
        for e in walkdir::WalkDir::new(&run.dir).follow_links(false).into_iter().flatten() {
            if !e.file_type().is_file() {
                continue;
            }
            // walkdir 是从 run.dir 往下走的，strip_prefix 不会失败；
            // 万一失败仍退回整条路径，与此前一致
            let rel = crate::foundation::path::to_rel(e.path(), &run.dir)
                .unwrap_or_else(|| e.path().to_string_lossy().replace('\\', "/"));
            if needle.is_empty() || rel.to_lowercase().contains(&needle) {
                out.push(Found {
                    run_id: run.id.clone(),
                    at_ms: run.at_ms,
                    stored: e.path().to_path_buf(),
                    rel,
                    size: e.metadata().map(|m| m.len()).unwrap_or(0),
                });
            }
        }
    }
    out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));
    out
}

/// 找回：把回收目录里的文件放回 `dest_root/<rel>`。
/// `run_id = None` 取最新一版。默认 dry-run。
/// 目标已存在时**不覆盖**（要覆盖请先自行挪开）——回收站的用途是找回，不是再来一次破坏。
pub fn restore(
    needle: &str,
    run_id: Option<&str>,
    dest_root: &Path,
    dry_run: bool,
) -> std::io::Result<(u64, u64, u64)> {
    let hits: Vec<Found> = find(needle)
        .into_iter()
        .filter(|f| run_id.map(|r| f.run_id == r).unwrap_or(true))
        .collect();
    if hits.is_empty() {
        return Ok((0, 0, 0));
    }
    // 每个 rel 只取最新的一版
    let mut seen = std::collections::HashSet::new();
    let mut restored = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;
    for f in hits {
        if !seen.insert(f.rel.clone()) {
            continue;
        }
        let dst = crate::foundation::path::join_native(dest_root, &f.rel);
        if dst.exists() {
            crate::log_warn!("trash", "skip (exists): {}", dst.display());
            skipped += 1;
            continue;
        }
        if dry_run {
            println!("WOULD RESTORE  {}  <-  {} ({})", dst.display(), f.run_id, crate::foundation::fmt::human_bytes(f.size));
            skipped += 1;
            continue;
        }
        let res = (|| -> std::io::Result<()> {
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            // 原子落盘：找回过程本身也不该留半截文件
            let mut st = crate::atomic::Staged::create(&dst)?;
            let mut src = std::fs::File::open(&f.stored)?;
            st.write_all_from(&mut src)?;
            st.seal(true)?;
            st.commit()
        })();
        match res {
            Ok(_) => {
                println!("RESTORED  {}  <-  {}", dst.display(), f.run_id);
                restored += 1;
            }
            Err(e) => {
                crate::log_error!("trash", "ERR  {}: {e}", dst.display());
                errors += 1;
            }
        }
    }
    Ok((restored, skipped, errors))
}

// ---------- 保留策略 ----------

#[derive(Clone, Copy, Debug)]
pub struct Retention {
    /// 超过这个天数一律删。<=0 关闭
    pub keep_days: i64,
    /// 全部批次总字节上限，超了从最旧的开始删。0 关闭
    pub max_bytes: u64,
    /// 启用 staggered 稀释（比单纯按天保留聪明：近期密、远期疏）
    pub staggered: bool,
}

impl Default for Retention {
    fn default() -> Self {
        Retention { keep_days: 30, max_bytes: 10 * 1024 * 1024 * 1024, staggered: true }
    }
}

/// syncthing `staggered` 的区间表（`lib/versioner/staggered.go:47-53`）：
/// (相邻两版之间至少间隔 step 秒, 该区间覆盖到 end 秒之前)
const INTERVALS: [(i64, i64); 4] = [
    (30, 3600),                    // 第一小时：每 30 秒最多留一份
    (3600, 86_400),                // 当天：每小时一份
    (86_400, 30 * 86_400),         // 30 天内：每天一份
    (7 * 86_400, 365 * 86_400),    // 一年内：每周一份
];

/// 给定各批次的**秒级**时间戳（任意顺序），返回应当删除的那些时间戳。
/// 纯函数，便于单测——这是整个保留策略里唯一有算法含量的部分。
pub fn staggered_removals(times_secs: &[i64], now_secs: i64) -> Vec<i64> {
    let mut times: Vec<i64> = times_secs.to_vec();
    times.sort_unstable(); // 从旧到新
    let mut remove = Vec::new();
    let mut prev_age = 0i64;
    let mut first = true;
    let max_age = INTERVALS[INTERVALS.len() - 1].1;
    for t in times {
        let age = now_secs - t;
        if max_age > 0 && age > max_age {
            remove.push(t); // 超过最大保留期
            continue;
        }
        if first {
            // 最旧的一份无条件保留，作为后续间隔判断的锚点
            prev_age = age;
            first = false;
            continue;
        }
        let step = INTERVALS.iter().find(|(_, end)| age < *end).map(|(s, _)| *s).unwrap_or(max_age);
        if prev_age - age < step {
            // 与上一份保留者挨得太近 → 稀释掉
            remove.push(t);
            continue;
        }
        prev_age = age;
    }
    remove
}

/// 按保留策略清理。返回 (删除批次数, 释放字节)。
pub fn prune(r: &Retention, dry_run: bool) -> std::io::Result<(u64, u64)> {
    let runs = list_runs();
    if runs.is_empty() {
        return Ok((0, 0));
    }
    let now_ms = crate::foundation::time::now_ms() as i64;
    let mut doomed: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1) 超龄
    if r.keep_days > 0 {
        let cutoff = now_ms - r.keep_days * 86_400_000;
        for run in &runs {
            if (run.at_ms as i64) < cutoff {
                doomed.insert(run.id.clone());
            }
        }
    }

    // 2) staggered 稀释
    if r.staggered {
        let times: Vec<i64> = runs.iter().map(|x| (x.at_ms / 1000) as i64).collect();
        for t in staggered_removals(&times, now_ms / 1000) {
            if let Some(run) = runs.iter().find(|x| (x.at_ms / 1000) as i64 == t) {
                doomed.insert(run.id.clone());
            }
        }
    }

    // 3) 总体积上限：从最旧的开始淘汰，直到降到线下
    if r.max_bytes > 0 {
        let mut total: u64 = runs.iter().filter(|x| !doomed.contains(&x.id)).map(|x| x.bytes).sum();
        for run in runs.iter() {
            if total <= r.max_bytes {
                break;
            }
            if doomed.insert(run.id.clone()) {
                total = total.saturating_sub(run.bytes);
            }
        }
    }

    // 永远保留最新一批：它最可能就是刚刚那次误操作的后悔药
    if let Some(newest) = runs.last() {
        doomed.remove(&newest.id);
    }

    let mut n = 0u64;
    let mut freed = 0u64;
    for run in &runs {
        if !doomed.contains(&run.id) {
            continue;
        }
        if dry_run {
            println!("WOULD PRUNE  {}  ({} files, {})", run.id, run.files, crate::foundation::fmt::human_bytes(run.bytes));
        } else if let Err(e) = std::fs::remove_dir_all(&run.dir) {
            crate::log_error!("trash", "ERR  prune {}: {e}", run.dir.display());
            continue;
        }
        n += 1;
        freed += run.bytes;
    }
    Ok((n, freed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;
    const D: i64 = 86_400;

    #[test]
    fn staggered_drops_over_max_age() {
        let now = 1_000_000_000;
        let times = vec![now - 400 * D, now - 10];
        let rm = staggered_removals(&times, now);
        assert!(rm.contains(&(now - 400 * D)), "older than a year must go");
        assert!(!rm.contains(&(now - 10)));
    }

    #[test]
    fn staggered_thins_dense_recent_runs() {
        let now = 1_000_000_000;
        // 第一小时内每 10 秒一份 → 区间要求 30 秒，应该被稀释掉大半
        let times: Vec<i64> = (0..12).map(|i| now - i * 10).collect();
        let rm = staggered_removals(&times, now);
        let kept = times.len() - rm.len();
        assert!(kept >= 3 && kept <= 6, "12 runs 10s apart should thin to ~4, got {kept}");
    }

    #[test]
    fn staggered_keeps_well_spaced_runs() {
        let now = 1_000_000_000;
        // 当天区间要求 1 小时间隔；这里给 2 小时间隔 → 一份都不该删
        let times: Vec<i64> = (1..10).map(|i| now - i * 2 * H).collect();
        let rm = staggered_removals(&times, now);
        assert!(rm.is_empty(), "runs spaced wider than the interval must all survive, removed {rm:?}");
    }

    #[test]
    fn staggered_always_keeps_the_oldest_anchor() {
        let now = 1_000_000_000;
        let times = vec![now - 5 * D, now - 5 * D + 1, now - 5 * D + 2];
        let rm = staggered_removals(&times, now);
        assert!(!rm.contains(&(now - 5 * D)), "oldest is the anchor and must survive");
        assert_eq!(rm.len(), 2, "the two that crowd it must go");
    }

    #[test]
    fn empty_input_is_fine() {
        assert!(staggered_removals(&[], 0).is_empty());
    }
}
