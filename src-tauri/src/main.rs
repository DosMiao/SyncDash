//! SyncDash 桌面壳（Tauri v2）：只做 IPC 编排，全部同步逻辑在 syncdash 核心库。
//! - compare_job 分阶段发 progress 事件（scan-source / scan-target / comparing）
//! - 每个 op 预计算 reverse_op（前端"点徽章翻方向"零逻辑漂移）
//! - apply_job 接收前端最终定稿的 op 列表（已含翻向与勾选），重活全走 spawn_blocking

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use syncdash::compare::{self, Action, Op, Plan, PlanHeader};
use syncdash::table::Snapshot;
use syncdash::{config, run, scan};
use tauri::Emitter;

#[derive(Serialize)]
struct JobDto {
    name: String,
    mode: String,
    rigor: String,
    source: String,
    target: String,
    has_archive: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct PlanDto {
    header: PlanHeader,
    ops: Vec<Op>,
    /// 与 ops 一一对应：可翻方向的行给出反向 op，不可翻为 null
    reversed: Vec<Option<Op>>,
}

#[derive(Serialize)]
struct ApplyDto {
    done: u64,
    skipped: u64,
    errors: u64,
}

#[derive(Serialize)]
struct PreflightDto {
    ok: bool,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Serialize, Clone)]
struct Progress {
    phase: String,
    detail: String,
    /// 哈希阶段的完成百分比（0-100）；非哈希阶段为 -1
    pct: i32,
    /// MiB/s，非哈希阶段为 0
    rate: f64,
}

fn emit_progress(app: &tauri::AppHandle, phase: &str, detail: String) {
    let _ = app.emit("progress", Progress { phase: phase.into(), detail, pct: -1, rate: 0.0 });
}

/// 把 scan 的字节级进度转成前端事件（P2-6）。大树扫描时用户能看见百分比与速率，
/// 而不是盯着一个不动的"扫描中"。
fn emit_scan_progress(app: &tauri::AppHandle, phase: &'static str, p: scan::ScanProgress) {
    let pct = if p.bytes_total > 0 { (p.bytes_done * 100 / p.bytes_total) as i32 } else { 100 };
    let _ = app.emit(
        "progress",
        Progress {
            phase: phase.into(),
            detail: format!(
                "{} / {}",
                syncdash::preflight::human_bytes(p.bytes_done),
                syncdash::preflight::human_bytes(p.bytes_total)
            ),
            pct,
            rate: p.mib_per_s,
        },
    );
}

#[tauri::command]
fn list_jobs() -> Vec<JobDto> {
    config::load_all()
        .into_iter()
        .map(|(name, j)| JobDto {
            name,
            mode: j.mode.clone(),
            rigor: j.rigor.clone(),
            source: j.source.display().to_string(),
            target: j.target.display().to_string(),
            has_archive: j.archive.is_some(),
        })
        .collect()
}

#[tauri::command]
fn jobs_dir() -> String {
    config::jobs_dir().display().to_string()
}

#[tauri::command]
async fn compare_job(app: tauri::AppHandle, name: String) -> Result<PlanDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        // P0-2：root 可达性 + 挂载点标记。共享盘没挂上时照常比对会产出
        // "把对面删光"的计划，这道闸必须在扫描之前。
        let mut v = syncdash::preflight::Verdict { blockers: Vec::new(), warnings: Vec::new() };
        syncdash::preflight::check_root("source", &job.source, job.require_marker, &mut v);
        syncdash::preflight::check_root("target", &job.target, job.require_marker, &mut v);
        for w in &v.warnings {
            emit_progress(&app, "warning", w.clone());
        }
        if !v.ok() {
            return Err(v.blockers.join("\n"));
        }
        let opt = run::scan_opts(&job);
        emit_progress(&app, "scan-source", job.source.display().to_string());
        let s = {
            let cb = |p: scan::ScanProgress| emit_scan_progress(&app, "scan-source", p);
            scan::scan_with_progress(&job.source, &opt, Some(&cb)).map_err(|e| e.to_string())?
        };
        emit_progress(&app, "scan-target", job.target.display().to_string());
        let t = {
            let cb = |p: scan::ScanProgress| emit_scan_progress(&app, "scan-target", p);
            scan::scan_with_progress(&job.target, &opt, Some(&cb)).map_err(|e| e.to_string())?
        };
        emit_progress(&app, "comparing", format!("{} × {} 条目", s.header.entry_count, t.header.entry_count));
        let archive = match (&job.archive, job.mode.as_str()) {
            (Some(p), "sync") if p.is_file() => Some(Snapshot::load(p).map_err(|e| e.to_string())?),
            _ => None,
        };
        let plan = compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &job.compare_opts());
        let reversed = plan.ops.iter().map(compare::reverse_op).collect();
        Ok(PlanDto { header: plan.header, ops: plan.ops, reversed })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 同步前的闸门体检（磁盘空间 / 删除占比）。前端在确认单里展示结果，
/// 让"为什么不让我同步"这句话有地方说，而不是只出现在 stderr。
#[tauri::command]
async fn preflight(name: String, plan: PlanDto, ops: Vec<Op>, acknowledged: bool) -> Result<PreflightDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        let full = Plan { header: plan.header.clone(), ops: plan.ops.clone() };
        let ops: Vec<Op> = ops
            .into_iter()
            .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
            .collect();
        let v = run::preflight_job(&job, &full, &ops, acknowledged);
        Ok(PreflightDto { ok: v.ok(), blockers: v.blockers, warnings: v.warnings })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn apply_job(name: String, plan: PlanDto, ops: Vec<Op>, acknowledged: bool) -> Result<ApplyDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        let full = Plan { header: plan.header.clone(), ops: plan.ops.clone() };
        let ops: Vec<Op> = ops
            .into_iter()
            .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
            .collect();
        // 闸门不通过时不动手，并把理由回传给界面
        let v = run::preflight_job(&job, &full, &ops, acknowledged);
        if !v.ok() {
            return Err(v.blockers.join("\n"));
        }
        let (done, skipped, errors) = run::apply_job_guarded(&job, &full, &ops, None, false, acknowledged);
        Ok(ApplyDto { done, skipped, errors })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![list_jobs, jobs_dir, compare_job, preflight, apply_job])
        .run(tauri::generate_context!())
        .expect("error while running SyncDash");
}
