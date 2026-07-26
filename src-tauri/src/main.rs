//! SyncDash 桌面壳（Tauri v2）：只做 IPC 编排，全部同步逻辑在 syncdash 核心库。
//! 命令都是薄封装：list_jobs / compare_job / apply_job，重活丢 spawn_blocking，不卡 UI。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use syncdash::compare::{Action, Op, Plan, PlanHeader};
use syncdash::{config, run};

#[derive(Serialize)]
struct JobDto {
    name: String,
    mode: String,
    source: String,
    target: String,
    has_archive: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct PlanDto {
    header: PlanHeader,
    ops: Vec<Op>,
}

#[derive(Serialize)]
struct ApplyDto {
    done: u64,
    skipped: u64,
    errors: u64,
}

#[tauri::command]
fn list_jobs() -> Vec<JobDto> {
    config::load_all()
        .into_iter()
        .map(|(name, j)| JobDto {
            name,
            mode: j.mode.clone(),
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
async fn compare_job(name: String) -> Result<PlanDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        let plan = run::compare_job(&job).map_err(|e| e.to_string())?;
        Ok(PlanDto { header: plan.header, ops: plan.ops })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn apply_job(name: String, plan: PlanDto, selected: Vec<usize>) -> Result<ApplyDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        let full = Plan { header: plan.header.clone(), ops: plan.ops.clone() };
        let ops: Vec<Op> = selected
            .into_iter()
            .filter_map(|i| plan.ops.get(i).cloned())
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
