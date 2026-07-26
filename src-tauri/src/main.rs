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

#[derive(Serialize, Clone)]
struct Progress {
    phase: String,
    detail: String,
}

fn emit_progress(app: &tauri::AppHandle, phase: &str, detail: String) {
    let _ = app.emit("progress", Progress { phase: phase.into(), detail });
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
        for (label, r) in [("source", &job.source), ("target", &job.target)] {
            if !r.is_dir() {
                return Err(format!("{label} 不可访问：{}", r.display()));
            }
        }
        let opt = run::scan_opts(&job);
        emit_progress(&app, "scan-source", job.source.display().to_string());
        let s = scan::scan(&job.source, &opt).map_err(|e| e.to_string())?;
        emit_progress(&app, "scan-target", job.target.display().to_string());
        let t = scan::scan(&job.target, &opt).map_err(|e| e.to_string())?;
        emit_progress(&app, "comparing", format!("{} × {} 条目", s.header.entry_count, t.header.entry_count));
        let archive = match (&job.archive, job.mode.as_str()) {
            (Some(p), "sync") if p.is_file() => Some(Snapshot::load(p).map_err(|e| e.to_string())?),
            _ => None,
        };
        let copts = compare::CompareOptions { case_insensitive: !job.case_sensitive };
        let plan = compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &copts);
        let reversed = plan.ops.iter().map(compare::reverse_op).collect();
        Ok(PlanDto { header: plan.header, ops: plan.ops, reversed })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn apply_job(name: String, plan: PlanDto, ops: Vec<Op>) -> Result<ApplyDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        let full = Plan { header: plan.header.clone(), ops: plan.ops.clone() };
        let ops: Vec<Op> = ops
            .into_iter()
            .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
            .collect();
        let (done, skipped, errors) = run::apply_job(&job, &full, &ops, None, false);
        Ok(ApplyDto { done, skipped, errors })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![list_jobs, jobs_dir, compare_job, apply_job])
        .run(tauri::generate_context!())
        .expect("error while running SyncDash");
}
