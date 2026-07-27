//! v0.10：app 级设置。
//!
//! 今天项目里只有 per-job TOML（`config.rs`）与前端 localStorage——"日志目录可配置"
//! 需要一个 app 级落点，这是第一个。位置：`<config>/settings.toml`，
//! 与 `config::jobs_dir()` 同级。
//!
//! 规矩同 `runlog`：**设置读不出来不该拦住同步**。解析失败、目录不可写，一律回退到
//! 能用的值并留一条日志，绝不向上抛。

use crate::progress::LogLevel;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct AppSettings {
    /// 空 = 默认 `<config>/logs`
    #[serde(default)]
    pub log_dir: String,
    /// 低于这个等级的 `Log` 事件不落盘
    #[serde(default = "default_level")]
    pub level: LogLevel,
    /// 超过这个天数的运行记录会被清理；0 = 不按天清
    #[serde(default = "default_keep_days")]
    #[ts(type = "number")]
    pub keep_days: u64,
    /// 日志总量上限（MB），超了从最旧的删起；0 = 不限
    #[serde(default = "default_max_total_mb")]
    #[ts(type = "number")]
    pub max_total_mb: u64,
    /// compare 运行的记录粒度：`summary`（只写索引行，不建目录）| `off`。
    /// 不给 `full` 档：watch 30s 一轮 = 一天 2880 次，建目录会把日志盘冲垮。
    #[serde(default = "default_log_compare")]
    pub log_compare: String,
    /// CLI：日志同时按原文进 stderr（保持改造前的终端体验）
    #[serde(default = "default_true")]
    pub mirror_stderr: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            log_dir: String::new(),
            level: default_level(),
            keep_days: default_keep_days(),
            max_total_mb: default_max_total_mb(),
            log_compare: default_log_compare(),
            mirror_stderr: true,
        }
    }
}

fn default_level() -> LogLevel {
    LogLevel::Info
}
fn default_keep_days() -> u64 {
    30
}
fn default_max_total_mb() -> u64 {
    512
}
fn default_log_compare() -> String {
    "summary".into()
}
fn default_true() -> bool {
    true
}

/// 配置根 = jobs 目录的父级（`jobs_dir()` 是 `<config>/jobs`）
pub fn config_dir() -> PathBuf {
    crate::config::jobs_dir().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.toml")
}

pub fn default_log_dir() -> PathBuf {
    config_dir().join("logs")
}

impl AppSettings {
    /// 配置里写的目录（未必存在/可写）。迁移时要拿它当"旧位置"，所以单列。
    pub fn wanted_log_dir(&self) -> PathBuf {
        if self.log_dir.trim().is_empty() {
            default_log_dir()
        } else {
            PathBuf::from(self.log_dir.trim())
        }
    }

    /// 实际可用的日志目录：配置值 → 建不出来退默认 → 默认也不行退临时目录。
    /// **绝不返回一个写不了的路径**——日志系统要么能写，要么安静地换个地方写。
    pub fn resolved_log_dir(&self) -> PathBuf {
        let want = self.wanted_log_dir();
        if std::fs::create_dir_all(&want).is_ok() {
            return want;
        }
        let fallback = default_log_dir();
        if want != fallback && std::fs::create_dir_all(&fallback).is_ok() {
            eprintln!("settings: 日志目录 {} 不可用，回退 {}", want.display(), fallback.display());
            return fallback;
        }
        std::env::temp_dir().join("syncdash-logs")
    }

    pub fn logs_compare(&self) -> bool {
        self.log_compare != "off"
    }
}

/// 读设置。文件不存在或读坏了都回默认。
pub fn load() -> AppSettings {
    let p = settings_path();
    match std::fs::read_to_string(&p) {
        Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
            eprintln!("settings: {} 解析失败，改用默认值：{e}", p.display());
            AppSettings::default()
        }),
        Err(_) => AppSettings::default(),
    }
}

pub fn save(s: &AppSettings) -> std::io::Result<PathBuf> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let text = toml::to_string_pretty(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("toml serialize: {e}")))?;
    let p = settings_path();
    std::fs::write(&p, text)?;
    Ok(p)
}

// ---------- 改日志目录时的整体迁移 ----------

#[derive(Serialize, Deserialize, Default, Debug, Clone, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct MigrateReport {
    #[ts(type = "number")]
    pub moved: u64,
    #[ts(type = "number")]
    pub skipped: u64,
    #[ts(type = "number")]
    pub failed: u64,
    /// 人话说明，界面直接贴出来
    pub messages: Vec<String>,
}

/// 把 `old` 下的运行目录与索引搬到 `new`。全程 best-effort：
/// - 同目录 / 旧目录不存在 → 直接返回
/// - 目标已有同名运行目录 → 跳过（不覆盖别人的历史）
/// - `runs.jsonl` 两边都有 → 按 ts_ms 归并重写
/// - 跨盘 `rename` 失败 → copy + delete 兜底（Windows 上跨卷 rename 必失败）
pub fn migrate_log_dir(old: &Path, new: &Path) -> MigrateReport {
    let mut r = MigrateReport::default();
    if old == new || !old.is_dir() {
        return r;
    }
    if let Err(e) = std::fs::create_dir_all(new) {
        r.failed += 1;
        r.messages.push(format!("目标目录建不出来 {}：{e}", new.display()));
        return r;
    }
    let Ok(rd) = std::fs::read_dir(old) else {
        r.failed += 1;
        r.messages.push(format!("旧目录读不了：{}", old.display()));
        return r;
    };
    for e in rd.flatten() {
        let from = e.path();
        let Some(name) = from.file_name().map(|n| n.to_os_string()) else {
            continue;
        };
        let to = new.join(&name);
        if to.exists() {
            if name == crate::foundation::names::RUNLOG_INDEX_FILE {
                merge_index(&from, &to, &mut r);
            } else {
                r.skipped += 1;
            }
            continue;
        }
        move_entry(&from, &to, &mut r);
    }
    if r.moved > 0 || r.failed > 0 {
        r.messages.push(format!("迁移完成：{} 搬运，{} 跳过，{} 失败", r.moved, r.skipped, r.failed));
    }
    r
}

/// 归并两份索引。索引是追加式 JSONL，`latest_by_job` 靠"后写的更新"取最近一次运行——
/// 首尾相接会让这个判据失真，所以按 ts_ms 排序后重写。
fn merge_index(from: &Path, into: &Path, r: &mut MigrateReport) {
    let mut lines: Vec<(i64, String)> = Vec::new();
    for p in [from, into] {
        let Ok(t) = std::fs::read_to_string(p) else {
            continue;
        };
        for l in t.lines() {
            let l = l.trim();
            if l.is_empty() {
                continue;
            }
            let ts = serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| v["ts_ms"].as_i64())
                .unwrap_or(0);
            lines.push((ts, l.to_string()));
        }
    }
    lines.sort_by_key(|(ts, _)| *ts);
    let body: String = lines.iter().map(|(_, l)| format!("{l}\n")).collect();
    match std::fs::write(into, body) {
        Ok(_) => {
            let _ = std::fs::remove_file(from);
            r.moved += 1;
        }
        Err(e) => {
            r.failed += 1;
            r.messages.push(format!("索引归并失败：{e}"));
        }
    }
}

fn move_entry(from: &Path, to: &Path, r: &mut MigrateReport) {
    // 同卷 rename 是原子且瞬时的；跨卷（换到别的盘就是）必失败，落到 copy+delete
    if std::fs::rename(from, to).is_ok() {
        r.moved += 1;
        return;
    }
    let copied = if from.is_dir() { copy_dir(from, to) } else { std::fs::copy(from, to).map(|_| ()) };
    match copied {
        Ok(_) => {
            let removed = if from.is_dir() { std::fs::remove_dir_all(from) } else { std::fs::remove_file(from) };
            if let Err(e) = removed {
                // 复制成功但旧件删不掉：新位置数据是齐的，不算失败——说清楚就行
                r.messages.push(format!("已复制但旧件删不掉 {}：{e}", from.display()));
            }
            r.moved += 1;
        }
        Err(e) => {
            r.failed += 1;
            r.messages.push(format!("搬运失败 {}：{e}", from.display()));
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let (f, t) = (e.path(), to.join(e.file_name()));
        if f.is_dir() {
            copy_dir(&f, &t)?;
        } else {
            std::fs::copy(&f, &t)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("syncdash-set-{tag}-{}", crate::foundation::time::now_ms()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        let s = AppSettings { log_dir: "D:\\logs".into(), level: LogLevel::Warn, keep_days: 7, ..Default::default() };
        let text = toml::to_string_pretty(&s).unwrap();
        let back: AppSettings = toml::from_str(&text).unwrap();
        assert_eq!(s, back);
        // 缺字段的老配置文件必须能读——每个字段都有 serde default
        let partial: AppSettings = toml::from_str("log_dir = ''").unwrap();
        assert_eq!(partial.level, LogLevel::Info);
        assert_eq!(partial.keep_days, 30);
        assert!(partial.mirror_stderr);
        assert!(partial.logs_compare());
    }

    #[test]
    fn migrate_moves_dirs_and_skips_collisions() {
        let root = tmp("mig");
        let (old, new) = (root.join("old"), root.join("new"));
        std::fs::create_dir_all(old.join("20260101-000000-a-apply")).unwrap();
        std::fs::write(old.join("20260101-000000-a-apply").join("run.jsonl"), "x\n").unwrap();
        std::fs::create_dir_all(old.join("20260102-000000-b-apply")).unwrap();
        // 目标已有同名 → 必须跳过，不能覆盖别人的历史
        std::fs::create_dir_all(new.join("20260102-000000-b-apply")).unwrap();
        std::fs::write(new.join("20260102-000000-b-apply").join("keep"), "mine").unwrap();

        let r = migrate_log_dir(&old, &new);
        assert_eq!(r.moved, 1, "只搬 a；b 撞名跳过：{r:?}");
        assert_eq!(r.skipped, 1);
        assert_eq!(r.failed, 0);
        assert!(new.join("20260101-000000-a-apply").join("run.jsonl").is_file());
        assert!(new.join("20260102-000000-b-apply").join("keep").is_file(), "撞名的一份不能被覆盖");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_merges_index_in_time_order() {
        let root = tmp("idx");
        let (old, new) = (root.join("old"), root.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let idx = crate::foundation::names::RUNLOG_INDEX_FILE;
        std::fs::write(old.join(idx), "{\"ts_ms\":10,\"job\":\"a\"}\n{\"ts_ms\":30,\"job\":\"a\"}\n").unwrap();
        std::fs::write(new.join(idx), "{\"ts_ms\":20,\"job\":\"b\"}\n").unwrap();

        migrate_log_dir(&old, &new);
        let merged = std::fs::read_to_string(new.join(idx)).unwrap();
        let ts: Vec<i64> = merged
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["ts_ms"].as_i64().unwrap())
            .collect();
        assert_eq!(ts, vec![10, 20, 30], "归并后必须按时间有序——latest_by_job 靠后写的更新");
        assert!(!old.join(idx).exists(), "归并后旧索引应删掉");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_same_dir_is_noop() {
        let root = tmp("noop");
        let r = migrate_log_dir(&root, &root);
        assert_eq!((r.moved, r.skipped, r.failed), (0, 0, 0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolved_log_dir_falls_back_when_unwritable() {
        // 把一个**文件**当目录填进设置：create_dir_all 必失败 → 必须回退到能写的地方
        let root = tmp("fallback");
        let f = root.join("not-a-dir");
        std::fs::write(&f, "x").unwrap();
        let s = AppSettings { log_dir: f.display().to_string(), ..Default::default() };
        let got = s.resolved_log_dir();
        assert_ne!(got, f, "不能返回一个写不了的路径");
        assert!(got.is_dir(), "回退目标必须真的存在：{}", got.display());
        let _ = std::fs::remove_dir_all(&root);
    }
}
