//! SyncDash 桌面壳（Tauri v2）：只做 IPC 编排，全部同步逻辑在 syncdash 核心库。
//! - v0.9 M1：统一事件流 —— 引擎经 `run::*_with(ctx)` 发 ProgressEvent，
//!   TauriSink 节流后以 `run-progress` 事件（带 run_id）发往前端；
//!   旧 `progress` 事件由 shim 从 PhaseStart/Progress 合成，M2 前端落地后拆除。
//! - 单运行互斥：RunState.active 持 RunCtl；cancel_run / pause_run 对它生效。
//! - 每个 op 预计算 reverse_op（前端"点徽章翻方向"零逻辑漂移）
//! - apply_job 接收前端最终定稿的 op 列表（已含翻向与勾选），重活全走 spawn_blocking

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use syncdash::compare::{self, Action, Op, Plan, PlanHeader};
use syncdash::progress::{Phase, ProgressEvent, ProgressSink, RunCtl, RunCtx};
use syncdash::{config, run};
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
    bytes_copied: u64,
    cancelled: bool,
}

#[derive(Serialize)]
struct PreflightDto {
    ok: bool,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

// ---------- 运行状态（互斥 + 取消/暂停句柄） ----------

#[derive(Default)]
struct RunState {
    active: Mutex<Option<Arc<RunCtl>>>,
    seq: AtomicU64,
}

fn begin_run(st: &RunState) -> Result<(u64, Arc<RunCtl>), String> {
    let mut g = st.active.lock().unwrap();
    if g.is_some() {
        return Err("另一个运行正在进行——先取消它或等它结束".into());
    }
    let ctl = RunCtl::new();
    *g = Some(ctl.clone());
    Ok((st.seq.fetch_add(1, Ordering::Relaxed) + 1, ctl))
}

fn end_run(st: &RunState) {
    *st.active.lock().unwrap() = None;
}

// ---------- 事件桥 ----------

/// 发往前端的 run-progress 载荷：run_id 让前端丢弃已取消运行的迟到事件
#[derive(Serialize, Clone)]
struct RunEvent {
    run_id: u64,
    #[serde(flatten)]
    ev: ProgressEvent,
}

/// 旧状态栏事件（M2 前端落地前的过渡 shim）
#[derive(Serialize, Clone)]
struct LegacyProgress {
    phase: String,
    detail: String,
    /// 哈希/复制阶段的完成百分比（0-100）；边界事件为 -1
    pct: i32,
    /// 保留字段：新流里速率归前端算（4s 滑窗），shim 不再伪造
    rate: f64,
}

fn legacy_phase(p: Phase) -> &'static str {
    match p {
        Phase::ScanSource => "scan-source",
        Phase::ScanTarget => "scan-target",
        Phase::Compare => "comparing",
        Phase::Apply => "applying",
        Phase::Pack => "packing",
        Phase::Ship => "shipping",
        Phase::Verify => "verifying",
        Phase::Refresh => "refreshing",
    }
}

fn legacy_shim(app: &tauri::AppHandle, ev: &ProgressEvent) {
    match ev {
        ProgressEvent::PhaseStart { phase, label, .. } => {
            let _ = app.emit(
                "progress",
                LegacyProgress {
                    phase: legacy_phase(*phase).into(),
                    detail: label.clone().unwrap_or_default(),
                    pct: -1,
                    rate: 0.0,
                },
            );
        }
        ProgressEvent::Progress { phase, bytes_done, bytes_total, items_done, items_total, .. } => {
            let pct = if *bytes_total > 0 {
                (bytes_done * 100 / bytes_total) as i32
            } else if *items_total > 0 {
                (items_done * 100 / items_total) as i32
            } else {
                -1
            };
            let _ = app.emit(
                "progress",
                LegacyProgress {
                    phase: legacy_phase(*phase).into(),
                    detail: format!(
                        "{} / {}",
                        syncdash::preflight::human_bytes(*bytes_done),
                        syncdash::preflight::human_bytes(*bytes_total)
                    ),
                    pct,
                    rate: 0.0,
                },
            );
        }
        _ => {}
    }
}

/// ProgressSink → Tauri 事件。Progress 类 ≥100ms/条节流（=FFS 图表采样率），
/// PhaseStart/Totals/Error/Paused/Resumed/Summary 直通。
struct TauriSink {
    app: tauri::AppHandle,
    run_id: u64,
    last_progress_ms: AtomicU64,
}

impl ProgressSink for TauriSink {
    fn emit(&self, ev: ProgressEvent) {
        if let ProgressEvent::Progress { ts_ms, .. } = &ev {
            let last = self.last_progress_ms.load(Ordering::Relaxed);
            if ts_ms.saturating_sub(last) < 100 {
                return;
            }
            self.last_progress_ms.store(*ts_ms, Ordering::Relaxed);
        }
        legacy_shim(&self.app, &ev);
        let _ = self.app.emit("run-progress", RunEvent { run_id: self.run_id, ev });
    }
}

fn make_ctx(app: &tauri::AppHandle, run_id: u64, ctl: Arc<RunCtl>) -> RunCtx {
    RunCtx::new(
        ctl,
        Arc::new(TauriSink { app: app.clone(), run_id, last_progress_ms: AtomicU64::new(0) }),
    )
}

fn user_err(e: std::io::Error) -> String {
    if syncdash::progress::is_cancelled(&e) { "cancelled".into() } else { e.to_string() }
}

// ---------- 命令 ----------

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

/// 对活动运行请求协作取消。返回是否存在活动运行。
#[tauri::command]
fn cancel_run(state: tauri::State<'_, Arc<RunState>>) -> bool {
    match state.active.lock().unwrap().as_ref() {
        Some(ctl) => {
            ctl.request_cancel();
            true
        }
        None => false,
    }
}

/// 暂停/恢复活动运行（暂停期间 elapsed 不涨、RootLock 心跳继续跳）
#[tauri::command]
fn pause_run(state: tauri::State<'_, Arc<RunState>>, paused: bool) -> bool {
    match state.active.lock().unwrap().as_ref() {
        Some(ctl) => {
            ctl.set_paused(paused);
            true
        }
        None => false,
    }
}

#[tauri::command]
async fn compare_job(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<RunState>>,
    name: String,
) -> Result<PlanDto, String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        let (run_id, ctl) = begin_run(&st)?;
        let ctx = make_ctx(&app, run_id, ctl);
        let r = run::compare_job_with(&job, &ctx);
        end_run(&st);
        let plan = r.map_err(user_err)?;
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
async fn apply_job(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<RunState>>,
    name: String,
    plan: PlanDto,
    ops: Vec<Op>,
    acknowledged: bool,
) -> Result<ApplyDto, String> {
    let st = state.inner().clone();
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
        let (run_id, ctl) = begin_run(&st)?;
        let ctx = make_ctx(&app, run_id, ctl);
        let out = run::apply_job_guarded_with(&job, &full, &ops, None, false, acknowledged, &ctx);
        end_run(&st);
        Ok(ApplyDto {
            done: out.done,
            skipped: out.skipped,
            errors: out.errors,
            bytes_copied: out.bytes_copied,
            cancelled: out.cancelled,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .manage(Arc::new(RunState::default()))
        .invoke_handler(tauri::generate_handler![
            list_jobs, jobs_dir, compare_job, preflight, apply_job, cancel_run, pause_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running SyncDash");
}
