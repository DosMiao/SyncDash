//! 运行日志（FFS 的 Log 列 / "Last sync" 列的数据源）。
//!
//! - 索引：`<log_dir>/runs.jsonl`——一行一次运行（追加式，人可读可审计）。
//! - 明细：`<log_dir>/<YYYYMMDD-HHMMSS>-<job>-<kind>/`——每次 apply 类运行一个目录：
//!   - `summary.json` 运行摘要（= 索引行的同一条记录，索引损坏时明细仍自解释）
//!   - `plan.jsonl`   计划清单：这次**打算**做什么
//!   - `run.jsonl`    事件流：叙事、阶段边界、报错
//!   - `errors.jsonl` 报错清单：Error + 警告以上的 Log
//!   - `items.jsonl`  执行清单：这次**实际**做成了什么（一行一 op 带结局）
//! - compare 无副作用：只写一行索引，不建目录（watch 30s 一轮 = 一天 2880 次）。
//! - 写日志绝不让同步失败：所有落盘都是 best-effort，失败进 stderr。
//!
//! v0.10 相对 v0.9 的两处要害改动：
//! 1. **流式**：事件到达即落盘（`logging::FileSink`），不再等 `finish` 一次性写——
//!    进程被杀时 v0.9 会丢掉整份日志。
//! 2. **计划与执行分开**：v0.9 写进明细的是传给 apply 的**计划 ops**，
//!    哪条成功、哪条失败、哪条 KEPT 一个字都没有。现在 plan/items 各一份。

use crate::compare::Op;
use crate::logging::{self, FileSink};
use crate::progress::{ApplyOutcome, LogLevel, ProgressEvent, ProgressSink, RunCtx};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::foundation::names::{
    RUNLOG_INDEX_FILE as INDEX_FILE, RUNLOG_PLAN_FILE as PLAN_FILE,
    RUNLOG_SUMMARY_FILE as SUMMARY_FILE,
};

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct RunRecord {
    /// 运行开始时刻（unix ms）
    #[ts(type = "number")]
    pub ts_ms: i64,
    pub job: String,
    /// "apply" | "remote-apply" | "compare" | "remote-compare"
    pub kind: String,
    #[ts(type = "number")]
    pub done: u64,
    #[ts(type = "number")]
    pub skipped: u64,
    #[ts(type = "number")]
    pub errors: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub elapsed_ms: u64,
    pub cancelled: bool,
    /// 运行目录名（相对 log_dir）。compare 类只有索引行、没有目录 → None
    #[serde(default)]
    pub run_id: Option<String>,
    /// 报错清单里的警告条数（错误条数在 `errors`）
    #[serde(default)]
    #[ts(type = "number")]
    pub warnings: u64,
    /// compare 类：发现的差异条数。apply 类为 None
    #[serde(default)]
    #[ts(type = "number | null")]
    pub ops_found: Option<u64>,
    /// 运行是否走完。`start` 先写一份 `finished:false` 的摘要，`finish` 再覆盖成 true——
    /// 中途被杀的运行在索引里没有行（`finish` 没跑到），只剩一个目录；
    /// 靠这个字段，那个目录仍然能自己说清"我没跑完"。
    /// 老记录默认 true：v0.9 只在 `finish` 里写记录，能写下来的都是走完的。
    #[serde(default = "yes")]
    pub finished: bool,
    /// v0.9 的明细文件名。新记录不再写，但老索引里有——读取端仍认。
    #[serde(default)]
    pub detail: Option<String>,
}

fn yes() -> bool {
    true
}

pub fn logs_dir() -> PathBuf {
    crate::settings::load().resolved_log_dir()
}

fn index_path() -> PathBuf {
    logs_dir().join(INDEX_FILE)
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// 拒绝任何能跳出 log_dir 的名字。`artifact_lines` / `detail_lines` 的入口守卫。
fn safe_component(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\') && !s.contains("..")
}


/// unix ms → `YYYYMMDD-HHMMSS`（UTC）。目录名要可排序，又不值得为它引 chrono/time。
/// 本地时间的显示交给前端（`main.ts` 已有 `relTime`），数据里始终是 `ts_ms`。
fn stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// days since 1970-01-01 → (year, month, day)。Howard Hinnant 的 `civil_from_days`。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}


/// 顺路统计报错清单的构成——summary 里"3 警告 2 错误"这句话的来源。
#[derive(Default)]
struct Tally {
    warnings: AtomicU64,
    errors: AtomicU64,
}

struct TallySink(Arc<Tally>);

impl ProgressSink for TallySink {
    fn emit(&self, ev: ProgressEvent) {
        match &ev {
            ProgressEvent::Log { level: LogLevel::Warn, .. } => {
                self.0.warnings.fetch_add(1, Ordering::Relaxed);
            }
            ProgressEvent::Log { level: LogLevel::Error, .. } | ProgressEvent::Error { .. } => {
                self.0.errors.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// 一次 apply 类运行的记录器。
///
/// `start` 把文件 sink 串进事件流**并装上进程级注册表**——后者让 `trash` / `version` /
/// `lock` 里那些拿不到 `RunCtx` 的诊断也能落进同一个目录。
/// `finish` 只写摘要与索引：明细在运行过程中已经落完了。
pub struct Recorder {
    pub ctx: RunCtx,
    name: String,
    kind: String,
    ts_ms: i64,
    run_id: Option<String>,
    file: Option<Arc<FileSink>>,
    tally: Arc<Tally>,
    /// drop 时自动把注册表还原——漏摘会让下一次运行的日志串进这个目录
    _guard: Option<crate::progress::SinkGuard>,
}

impl Recorder {
    pub fn start(name: &str, kind: &str, base: &RunCtx, ops: &[Op]) -> Recorder {
        let cfg = crate::settings::load();
        let ts_ms = crate::foundation::time::now_ms() as i64;
        let run_id = format!("{}-{}-{}", stamp(ts_ms), sanitize(name), sanitize(kind));
        let dir = cfg.resolved_log_dir().join(&run_id);
        let tally = Arc::new(Tally::default());

        // 不在这里串 `progress::current()`：`RunCtx::null()` 已经把进程环境 sink
        // （CLI 启动装的 StderrSink）带进 `base.sink` 了，再串一次就是重复输出。
        let mut sinks: Vec<Arc<dyn ProgressSink>> = vec![base.sink.clone(), Arc::new(TallySink(tally.clone()))];
        let (file, run_id) = match std::fs::create_dir_all(&dir) {
            Ok(_) => {
                write_plan(&dir, ops);
                // 先落一份 finished:false 的摘要——这样即使进程被杀，
                // 这个目录也能自己说清"我是谁、我没跑完"，而不是变成一堆匿名残片
                write_summary(&dir, &pending_record(name, kind, ts_ms, &run_id, ops.len() as u64));
                let f = Arc::new(FileSink::open(&dir, cfg.level));
                sinks.push(f.clone() as Arc<dyn ProgressSink>);
                (Some(f), Some(run_id))
            }
            Err(e) => {
                // 目录建不出来只丢明细，不丢同步——索引行照写
                eprintln!("runlog: 建不出 {}：{e}", dir.display());
                (None, None)
            }
        };

        let sink: Arc<dyn ProgressSink> = Arc::new(logging::MultiSink::new(sinks));
        let guard = crate::progress::install(sink.clone());
        Recorder {
            ctx: RunCtx::new(base.ctl.clone(), sink),
            name: name.to_string(),
            kind: kind.to_string(),
            ts_ms,
            run_id,
            file,
            tally,
            _guard: Some(guard),
        }
    }

    /// best-effort 落盘；返回写好的索引记录（desktop 直接回显"上次同步"用）
    pub fn finish(self, out: &ApplyOutcome, elapsed_ms: u64) -> RunRecord {
        // 先把缓冲全落盘，再写摘要——摘要存在就意味着明细已经齐了
        if let Some(f) = &self.file {
            f.flush_all();
        }
        let rec = RunRecord {
            ts_ms: self.ts_ms,
            job: self.name,
            kind: self.kind,
            done: out.done,
            skipped: out.skipped,
            errors: out.errors,
            bytes: out.bytes_copied,
            elapsed_ms,
            cancelled: out.cancelled,
            run_id: self.run_id.clone(),
            warnings: self.tally.warnings.load(Ordering::Relaxed),
            ops_found: None,
            finished: true,
            detail: None,
        };
        if let Some(id) = &self.run_id {
            // 覆盖 start 写下的 finished:false 占位
            write_summary(&logs_dir().join(id), &rec);
        }
        append_index(&rec);
        rec
    }
}

/// 运行开始时的占位摘要：计划规模已知，结果全空，`finished:false`。
fn pending_record(name: &str, kind: &str, ts_ms: i64, run_id: &str, planned: u64) -> RunRecord {
    RunRecord {
        ts_ms,
        job: name.to_string(),
        kind: kind.to_string(),
        done: 0,
        skipped: planned,
        errors: 0,
        bytes: 0,
        elapsed_ms: 0,
        cancelled: false,
        run_id: Some(run_id.to_string()),
        warnings: 0,
        ops_found: None,
        finished: false,
        detail: None,
    }
}

fn write_summary(dir: &Path, rec: &RunRecord) {
    match serde_json::to_string_pretty(rec) {
        Ok(t) => {
            if let Err(e) = std::fs::write(dir.join(SUMMARY_FILE), t) {
                eprintln!("runlog: 摘要写不进 {}：{e}", dir.display());
            }
        }
        Err(e) => eprintln!("runlog: 摘要序列化失败：{e}"),
    }
}

fn write_plan(dir: &Path, ops: &[Op]) {
    let write = || -> std::io::Result<()> {
        let f = std::fs::File::create(dir.join(PLAN_FILE))?;
        let mut w = std::io::BufWriter::new(f);
        for op in ops {
            writeln!(w, "{}", serde_json::json!({ "op": op }))?;
        }
        w.flush()
    };
    if let Err(e) = write() {
        eprintln!("runlog: 计划清单写失败：{e}");
    }
}

fn append_index(rec: &RunRecord) {
    let append = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(index_path())?;
        writeln!(f, "{}", serde_json::to_string(rec)?)
    };
    if let Err(e) = append() {
        eprintln!("runlog: 索引追加失败：{e}");
    }
}

/// compare 类运行的留痕：只追加一行索引，**不建目录**。
///
/// watch 模式 30s 一轮 = 一天 2880 次，每次建目录会把日志盘冲垮；
/// 而"什么时候比过、发现多少差异"这一行本身是有价值的。
pub fn compare_summary(name: &str, kind: &str, ts_ms: i64, ops_found: u64, elapsed_ms: u64, cancelled: bool) {
    if !crate::settings::load().logs_compare() {
        return;
    }
    append_index(&RunRecord {
        ts_ms,
        job: name.to_string(),
        kind: kind.to_string(),
        done: 0,
        skipped: 0,
        errors: 0,
        bytes: 0,
        elapsed_ms,
        cancelled,
        run_id: None,
        warnings: 0,
        ops_found: Some(ops_found),
        finished: true,
        detail: None,
    });
}


/// 历史（新→旧）。job = None 时全量；损坏行跳过不炸。
pub fn history(job: Option<&str>, limit: usize) -> Vec<RunRecord> {
    let Ok(text) = std::fs::read_to_string(index_path()) else {
        return Vec::new();
    };
    let mut out: Vec<RunRecord> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<RunRecord>(l.trim()).ok())
        .filter(|r| job.map(|j| r.job == j).unwrap_or(true))
        .collect();
    out.reverse();
    out.truncate(limit);
    out
}

/// 历史 + **被中断的运行**（新→旧）。
///
/// 索引行只在 `finish` 里追加，所以进程被杀的那次运行在索引里根本不存在——
/// 只剩一个目录。界面若只读索引，崩溃的运行就完全隐形，而那恰恰是最该被看见的一次。
/// 这里把目录里的 `summary.json`（`start` 写的 `finished:false` 占位）补进来。
pub fn history_merged(job: Option<&str>, limit: usize) -> Vec<RunRecord> {
    let mut out = history(job, usize::MAX);
    let known: std::collections::HashSet<String> =
        out.iter().filter_map(|r| r.run_id.clone()).collect();
    if let Ok(rd) = std::fs::read_dir(logs_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Some(id) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if known.contains(id) {
                continue;
            }
            let Ok(t) = std::fs::read_to_string(p.join(SUMMARY_FILE)) else {
                continue;
            };
            if let Ok(r) = serde_json::from_str::<RunRecord>(&t) {
                if job.map(|j| r.job == j).unwrap_or(true) {
                    out.push(r);
                }
            }
        }
    }
    out.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms));
    out.truncate(limit);
    out
}

/// 每个任务最近一次**真正执行**的运行（侧栏"上次同步"点）。
/// compare 行不参与——它没动过任何数据，拿它当"上次同步"是撒谎。
pub fn latest_by_job() -> std::collections::HashMap<String, RunRecord> {
    let mut m = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(index_path()) else {
        return m;
    };
    for l in text.lines() {
        if let Ok(r) = serde_json::from_str::<RunRecord>(l.trim()) {
            if r.ops_found.is_some() {
                continue;
            }
            m.insert(r.job.clone(), r); // 追加式文件：后写的天然更新
        }
    }
    m
}

/// 一次运行的某份产物（原样 JSONL 行；行数封顶防爆内存）。
/// `which` ∈ run / errors / items / plan / summary。
pub fn artifact_lines(run_id: &str, which: &str, max: usize) -> Vec<String> {
    if !safe_component(run_id) {
        return Vec::new();
    }
    let file = match which {
        "run" => FileSink::RUN,
        "errors" => FileSink::ERRORS,
        "items" => FileSink::ITEMS,
        "plan" => PLAN_FILE,
        "summary" => SUMMARY_FILE,
        _ => return Vec::new(),
    };
    let Ok(text) = std::fs::read_to_string(logs_dir().join(run_id).join(file)) else {
        return Vec::new();
    };
    text.lines().take(max).map(|s| s.to_string()).collect()
}

/// v0.9 老记录的明细文件（平铺在 log_dir 下的单个 jsonl）。
/// 新记录走 `artifact_lines`；这条留着让改造前的历史仍然打得开。
pub fn detail_lines(detail: &str, max: usize) -> Vec<String> {
    if !safe_component(detail) {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(logs_dir().join(detail)) else {
        return Vec::new();
    };
    text.lines().take(max).map(|s| s.to_string()).collect()
}


fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let path = e.path();
        n += if path.is_dir() { dir_size(&path) } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
    }
    n
}

/// 删一条运行记录的明细（老格式的平铺文件 / 新格式的目录）。
fn drop_detail(r: &RunRecord, root: &Path) {
    if let Some(id) = &r.run_id {
        if safe_component(id) {
            let _ = std::fs::remove_dir_all(root.join(id));
        }
    }
    if let Some(d) = &r.detail {
        if safe_component(d) {
            let _ = std::fs::remove_file(root.join(d));
        }
    }
}

/// 按天数 + 总量双条件清理。返回删掉的运行数。
///
/// `keep_days == 0` 关闭按天清；`max_total_mb == 0` 关闭按量清。
/// 执行清单全记（一次大同步上万行）——总量闸门是它的安全带。
pub fn prune(keep_days: u64, max_total_mb: u64) -> u64 {
    let root = logs_dir();
    let idx = root.join(INDEX_FILE);
    let Ok(text) = std::fs::read_to_string(&idx) else {
        return 0;
    };
    // 看不懂的行保守保留，但它们不参与体积核算
    let mut kept: Vec<(Option<RunRecord>, String)> = Vec::new();
    let mut dropped = 0u64;
    let cutoff = crate::foundation::time::now_ms() as i64 - (keep_days as i64) * 24 * 3600 * 1000;
    for l in text.lines() {
        let raw = l.trim();
        if raw.is_empty() {
            continue;
        }
        match serde_json::from_str::<RunRecord>(raw) {
            Ok(r) => {
                if keep_days > 0 && r.ts_ms < cutoff {
                    drop_detail(&r, &root);
                    dropped += 1;
                } else {
                    kept.push((Some(r), raw.to_string()));
                }
            }
            Err(_) => kept.push((None, raw.to_string())),
        }
    }

    // 总量闸门：还超就从最旧的删起（kept 是索引顺序 = 时间顺序）
    if max_total_mb > 0 {
        let cap = max_total_mb * 1024 * 1024;
        let mut sizes: Vec<u64> = kept
            .iter()
            .map(|(r, _)| match r.as_ref().and_then(|r| r.run_id.as_deref()) {
                Some(id) if safe_component(id) => dir_size(&root.join(id)),
                _ => 0,
            })
            .collect();
        let mut total: u64 = sizes.iter().sum();
        let mut i = 0;
        while total > cap && i < kept.len() {
            if let (Some(r), _) = &kept[i] {
                if r.run_id.is_some() || r.detail.is_some() {
                    drop_detail(r, &root);
                    total = total.saturating_sub(sizes[i]);
                    sizes[i] = 0;
                    kept[i].0 = None;
                    kept[i].1.clear(); // 标记删除
                    dropped += 1;
                }
            }
            i += 1;
        }
        kept.retain(|(_, raw)| !raw.is_empty());
    }

    if dropped > 0 {
        let body: String = kept.iter().map(|(_, l)| format!("{l}\n")).collect();
        if let Err(e) = std::fs::write(&idx, body) {
            eprintln!("runlog: 索引重写失败：{e}");
        }
        // 孤儿目录（崩溃留下的、索引里没有的）也扫掉。只碰早于 cutoff 的，
        // 免得把**正在跑**的那次运行的目录删了——它还没来得及写索引行。
        if keep_days > 0 {
            sweep_orphans(&root, &kept, cutoff);
        }
    }
    dropped
}

fn sweep_orphans(root: &Path, kept: &[(Option<RunRecord>, String)], cutoff: i64) {
    let live: std::collections::HashSet<&str> =
        kept.iter().filter_map(|(r, _)| r.as_ref().and_then(|r| r.run_id.as_deref())).collect();
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if live.contains(name) {
            continue;
        }
        let old_enough = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_millis() as i64) < cutoff)
            .unwrap_or(false);
        if old_enough {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip_keeps_old_fields_readable() {
        let a = RunRecord {
            ts_ms: 1,
            job: "j".into(),
            kind: "apply".into(),
            done: 3,
            skipped: 0,
            errors: 1,
            bytes: 42,
            elapsed_ms: 100,
            cancelled: false,
            run_id: Some("20260101-000000-j-apply".into()),
            warnings: 2,
            ops_found: None,
            finished: true,
            detail: None,
        };
        let s = serde_json::to_string(&a).unwrap();
        let b: RunRecord = serde_json::from_str(&s).unwrap();
        assert_eq!((b.done, b.errors, b.warnings), (3, 1, 2));
        // v0.9 写下的老索引行必须还能读（新字段全 serde default）
        let old = r#"{"ts_ms":9,"job":"j","kind":"apply","done":1,"skipped":0,"errors":0,
            "bytes":0,"elapsed_ms":5,"cancelled":false,"detail":"9-j.jsonl"}"#;
        let c: RunRecord = serde_json::from_str(old).unwrap();
        assert_eq!(c.detail.as_deref(), Some("9-j.jsonl"));
        assert!(c.run_id.is_none() && c.ops_found.is_none());
        // v0.9 只在 finish 里写记录，能写下来的都是走完的 → 老行读出来必须是 finished
        assert!(c.finished, "老索引行没有 finished 字段，默认必须是 true");
    }

    #[test]
    fn stamp_matches_known_unix_times() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(946_684_800_000), "20000101-000000"); // 2000-01-01T00:00:00Z
        assert_eq!(stamp(1_000_000_000_000), "20010909-014640"); // 经典的十亿秒
        assert_eq!(stamp(1_709_164_800_000), "20240229-000000"); // 闰日
    }

    #[test]
    fn stamps_sort_chronologically() {
        // 目录名要能直接按字典序排 —— 这是不引 chrono 的全部理由
        let mut v = vec![stamp(1_000_000_000_000), stamp(0), stamp(946_684_800_000)];
        v.sort();
        assert_eq!(v, vec!["19700101-000000", "20000101-000000", "20010909-014640"]);
    }

    #[test]
    fn path_escapes_are_refused() {
        assert!(!safe_component("../../etc/passwd"));
        assert!(!safe_component("a/b"));
        assert!(!safe_component("a\\b"));
        assert!(!safe_component(""));
        assert!(safe_component("20260101-000000-job-apply"));
        assert!(artifact_lines("../secrets", "run", 10).is_empty());
        assert!(artifact_lines("ok-id", "no-such-artifact", 10).is_empty());
    }

    #[test]
    fn sanitize_strips_path_chars() {
        assert!(sanitize("a b/c\\d").chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'));
    }
}
