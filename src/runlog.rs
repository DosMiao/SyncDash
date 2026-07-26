//! v0.9 M4：运行日志（FFS 的 Log 列 / "Last sync" 列的数据源）。
//!
//! - 索引：`<config>/logs/runs.jsonl`——一行一次运行（追加式，人可读可审计）。
//! - 明细：`<config>/logs/<ts>-<job>.jsonl`——该次运行的定稿 op 清单 + 累积错误。
//! - 只记 apply 类运行；compare 无副作用，不留痕。
//! - 写日志绝不让同步失败：所有落盘都是 best-effort，失败进 stderr。

use crate::compare::Op;
use crate::progress::{ApplyOutcome, ProgressEvent, ProgressSink, RunCtx};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunRecord {
    /// 运行开始时刻（unix ms）
    pub ts_ms: i64,
    pub job: String,
    /// "apply" | "remote-apply"
    pub kind: String,
    pub done: u64,
    pub skipped: u64,
    pub errors: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub cancelled: bool,
    /// 明细文件名（相对 logs/；索引损坏时明细仍独立可读）
    #[serde(default)]
    pub detail: Option<String>,
}

pub fn logs_dir() -> PathBuf {
    crate::config::jobs_dir().parent().map(|p| p.join("logs")).unwrap_or_else(|| PathBuf::from("logs"))
}

fn index_path() -> PathBuf {
    logs_dir().join("runs.jsonl")
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// 收集 Error 事件（转发给真正的 sink，同时留一份给明细文件）。
/// warning 不算错误，但一样值得留档。
pub struct ErrCollector {
    inner: Arc<dyn ProgressSink>,
    errors: Mutex<Vec<serde_json::Value>>,
}

impl ErrCollector {
    pub fn new(inner: Arc<dyn ProgressSink>) -> ErrCollector {
        ErrCollector { inner, errors: Mutex::new(Vec::new()) }
    }
}

impl ProgressSink for ErrCollector {
    fn emit(&self, ev: ProgressEvent) {
        if let ProgressEvent::Error { path, action, side, message, .. } = &ev {
            self.errors.lock().unwrap().push(serde_json::json!({
                "err": { "path": path, "action": action, "side": side, "message": message }
            }));
        }
        self.inner.emit(ev);
    }
}

/// 一次 apply 类运行的记录器：start 时把错误收集器插进事件流，
/// finish 时写索引行 + 明细文件。调用方持有任务名（Job 结构里没有名字）。
pub struct Recorder {
    pub ctx: RunCtx,
    name: String,
    kind: String,
    ts_ms: i64,
    coll: Arc<ErrCollector>,
}

impl Recorder {
    pub fn start(name: &str, kind: &str, base: &RunCtx) -> Recorder {
        let coll = Arc::new(ErrCollector::new(base.sink.clone()));
        let ctx = RunCtx::new(base.ctl.clone(), coll.clone() as Arc<dyn ProgressSink>);
        Recorder { ctx, name: name.to_string(), kind: kind.to_string(), ts_ms: crate::table::now_ms() as i64, coll }
    }

    /// best-effort 落盘；返回写好的索引记录（desktop 直接回显"上次同步"用）
    pub fn finish(self, out: &ApplyOutcome, ops: &[Op], elapsed_ms: u64) -> RunRecord {
        let detail_name = format!("{}-{}.jsonl", self.ts_ms, sanitize(&self.name));
        let mut rec = RunRecord {
            ts_ms: self.ts_ms,
            job: self.name,
            kind: self.kind,
            done: out.done,
            skipped: out.skipped,
            errors: out.errors,
            bytes: out.bytes_copied,
            elapsed_ms,
            cancelled: out.cancelled,
            detail: Some(detail_name.clone()),
        };
        let dir = logs_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("runlog: cannot create {}: {e}", dir.display());
            rec.detail = None;
            return rec;
        }
        // 明细：定稿 op 清单 + 错误
        let write_detail = || -> std::io::Result<()> {
            let mut f = std::fs::File::create(dir.join(&detail_name))?;
            for op in ops {
                writeln!(f, "{}", serde_json::json!({ "op": op }))?;
            }
            for e in self.coll.errors.lock().unwrap().iter() {
                writeln!(f, "{e}")?;
            }
            Ok(())
        };
        if let Err(e) = write_detail() {
            eprintln!("runlog: detail write failed: {e}");
            rec.detail = None;
        }
        let append = || -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(index_path())?;
            writeln!(f, "{}", serde_json::to_string(&rec)?)
        };
        if let Err(e) = append() {
            eprintln!("runlog: index append failed: {e}");
        }
        rec
    }
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

/// 每个任务最近一次运行（desktop 侧栏"上次同步"点）
pub fn latest_by_job() -> std::collections::HashMap<String, RunRecord> {
    let mut m = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(index_path()) else {
        return m;
    };
    for l in text.lines() {
        if let Ok(r) = serde_json::from_str::<RunRecord>(l.trim()) {
            m.insert(r.job.clone(), r); // 追加式文件：后写的天然更新
        }
    }
    m
}

/// 读一次运行的明细行（原样 JSONL 字符串，desktop/CLI 自行呈现）；行数封顶防爆内存
pub fn detail_lines(detail: &str, max: usize) -> Vec<String> {
    // 防路径逃逸：明细名只允许纯文件名
    if detail.contains('/') || detail.contains('\\') || detail.contains("..") {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(logs_dir().join(detail)) else {
        return Vec::new();
    };
    text.lines().take(max).map(|s| s.to_string()).collect()
}

/// 清理 N 天前的记录：重写索引 + 删对应明细。返回删除的运行数。
pub fn prune(keep_days: u64) -> u64 {
    let cutoff = crate::table::now_ms() as i64 - (keep_days as i64) * 24 * 3600 * 1000;
    let Ok(text) = std::fs::read_to_string(index_path()) else {
        return 0;
    };
    let mut kept = Vec::new();
    let mut dropped = 0u64;
    for l in text.lines() {
        match serde_json::from_str::<RunRecord>(l.trim()) {
            Ok(r) if r.ts_ms < cutoff => {
                if let Some(d) = &r.detail {
                    let _ = std::fs::remove_file(logs_dir().join(d));
                }
                dropped += 1;
            }
            Ok(_) => kept.push(l.to_string()),
            Err(_) => kept.push(l.to_string()), // 看不懂的行保守保留
        }
    }
    if dropped > 0 {
        let _ = std::fs::write(index_path(), kept.join("\n") + if kept.is_empty() { "" } else { "\n" });
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip_and_latest() {
        // 不碰真实 config 目录：只测序列化与 latest 逻辑所依赖的“后行覆盖”
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
            detail: None,
        };
        let s = serde_json::to_string(&a).unwrap();
        let b: RunRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(b.done, 3);
        assert_eq!(b.errors, 1);
        assert!(sanitize("a b/c").chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'));
    }
}
