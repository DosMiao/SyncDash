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
    // v0.9 M3：补齐前端需要感知的字段（remote 徽章 / versioning 标识 / 过滤器提示）
    remote: bool,
    remote_host: Option<String>,
    versioning: bool,
    delta: bool,
    parallel: Option<usize>,
    include: Vec<String>,
    exclude: Vec<String>,
    watch_interval_secs: Option<u64>,
    watch_auto_apply: bool,
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

/// 发往前端的 run-progress 载荷：run_id 让前端丢弃已取消运行的迟到事件；
/// purpose 区分 compare / apply——子窗口只认 apply（否则同步后的自动复核比对
/// 会把还开着的结果窗劫持成永远转圈的"比对中"），主窗内嵌面板只认 compare。
#[derive(Serialize, Clone)]
struct RunEvent {
    run_id: u64,
    purpose: &'static str,
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
    purpose: &'static str,
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
        let _ = self.app.emit("run-progress", RunEvent { run_id: self.run_id, purpose: self.purpose, ev });
    }
}

fn make_ctx(app: &tauri::AppHandle, run_id: u64, ctl: Arc<RunCtl>, purpose: &'static str) -> RunCtx {
    RunCtx::new(
        ctl,
        Arc::new(TauriSink { app: app.clone(), run_id, purpose, last_progress_ms: AtomicU64::new(0) }),
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
            remote: j.remote_host.is_some(),
            remote_host: j.remote_host.clone(),
            versioning: j.versioning,
            delta: j.delta,
            parallel: j.parallel,
            include: j.include.clone(),
            exclude: j.exclude.clone(),
            watch_interval_secs: j.watch_interval_secs,
            watch_auto_apply: j.watch_auto_apply,
        })
        .collect()
}

#[tauri::command]
fn jobs_dir() -> String {
    config::jobs_dir().display().to_string()
}

/// M5：编辑器读取完整 Job（list_jobs 的 DTO 只有摘要）
#[tauri::command]
fn get_job(name: String) -> Result<config::Job, String> {
    config::load(&name).map(|(_, j)| j).map_err(|e| e.to_string())
}

/// M5：保存任务（新建或覆盖同名 TOML）
#[tauri::command]
fn save_job(name: String, job: config::Job) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("任务名不能为空".into());
    }
    config::save_job(name.trim(), &job).map(|p| p.display().to_string()).map_err(|e| e.to_string())
}

/// M5：删除任务配置文件（数据一个字节都不动）
#[tauri::command]
fn delete_job(name: String) -> Result<(), String> {
    config::delete_job(&name).map_err(|e| e.to_string())
}

/// M4：运行历史（新→旧）。job = null 时看全部
#[tauri::command]
fn run_history(job: Option<String>, limit: Option<usize>) -> Vec<syncdash::runlog::RunRecord> {
    syncdash::runlog::history(job.as_deref(), limit.unwrap_or(50))
}

/// M4：每个任务最近一次运行——侧栏"上次同步"点的数据源
#[tauri::command]
fn last_syncs() -> std::collections::HashMap<String, syncdash::runlog::RunRecord> {
    syncdash::runlog::latest_by_job()
}

/// M4：某次运行的明细行（原样 JSONL；行数封顶）
#[tauri::command]
fn run_detail(detail: String) -> Vec<String> {
    syncdash::runlog::detail_lines(&detail, 2000)
}

/// 打开（或聚焦）独立进度子窗口（只用于 Synchronize；compare 进度在主窗原地显示）。
/// **必须是 async 命令**：同步命令在主线程的 IPC 里执行，而 wry 建窗要靠主事件循环
/// 泵消息——同步建窗会让子窗导航卡死在 about:blank（整窗纯白），关闭事件也排不上队
/// （表现为"整个 app 关不掉"）。async 命令跑在独立线程，建窗经事件循环正确代理。
#[tauri::command]
async fn open_progress_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("progress") {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(&app, "progress", tauri::WebviewUrl::App("progress.html".into()))
        .title("SyncDash — 运行")
        .inner_size(620.0, 500.0)
        .min_inner_size(440.0, 380.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 销毁进度子窗口。不是 hide：隐藏的子窗会在主窗关闭后让进程赖着不退
#[tauri::command]
async fn close_progress_window(app: tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("progress") {
        let _ = w.destroy();
    }
}

/// When finished 动作（FFS 同款）：sleep / shutdown。倒计时与确认都在前端做完才调这里。
#[tauri::command]
fn post_sync_action(kind: String) -> Result<(), String> {
    let (prog, args): (&str, Vec<&str>) = if cfg!(windows) {
        match kind.as_str() {
            "sleep" => ("rundll32.exe", vec!["powrprof.dll,SetSuspendState", "0,1,0"]),
            "shutdown" => ("shutdown", vec!["/s", "/t", "5"]),
            _ => return Ok(()),
        }
    } else {
        match kind.as_str() {
            "sleep" => ("pmset", vec!["sleepnow"]),
            "shutdown" => ("osascript", vec!["-e", "tell application \"System Events\" to shut down"]),
            _ => return Ok(()),
        }
    };
    std::process::Command::new(prog).args(&args).spawn().map(|_| ()).map_err(|e| e.to_string())
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
        let ctx = make_ctx(&app, run_id, ctl, "compare");
        // M3：remote 任务走远程管线（远端自己盘上扫描），不再静默落进本地管线
        let r = if job.remote_host.is_some() {
            run::compare_remote_job_with(&name, &job, &ctx)
        } else {
            run::compare_job_with(&job, &ctx)
        };
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
        // remote 任务只有删除占比闸门（磁盘空间/marker 在远端机器上，本地查了也是错的）
        let v = if job.remote_host.is_some() {
            run::preflight_remote_job(&job, &full, &ops, acknowledged)
        } else {
            run::preflight_job(&job, &full, &ops, acknowledged)
        };
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
        let v = if job.remote_host.is_some() {
            run::preflight_remote_job(&job, &full, &ops, acknowledged)
        } else {
            run::preflight_job(&job, &full, &ops, acknowledged)
        };
        if !v.ok() {
            return Err(v.blockers.join("\n"));
        }
        let (run_id, ctl) = begin_run(&st)?;
        let ctx = make_ctx(&app, run_id, ctl, "apply");
        // M4：每次真实 apply 落一条运行日志（Recorder 顺带把错误事件收进明细文件）
        let t0 = std::time::Instant::now();
        let remote = job.remote_host.is_some();
        let rec = syncdash::runlog::Recorder::start(&name, if remote { "remote-apply" } else { "apply" }, &ctx);
        let out = if remote {
            match run::apply_remote_job_with(&name, &job, &full, &ops, false, acknowledged, &rec.ctx) {
                Ok(o) => o,
                Err(e) => {
                    end_run(&st);
                    return Err(user_err(e));
                }
            }
        } else {
            run::apply_job_guarded_with(&job, &full, &ops, None, false, acknowledged, &rec.ctx)
        };
        rec.finish(&out, &ops, t0.elapsed().as_millis() as u64);
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
        // 主窗关闭 → 级联销毁进度子窗；否则残留窗口让 Tauri 不退出（"app 关不掉"）
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if window.label() == "main" {
                    use tauri::Manager;
                    if let Some(p) = window.app_handle().get_webview_window("progress") {
                        let _ = p.destroy();
                    }
                }
            }
        })
        .manage(Arc::new(RunState::default()))
        .invoke_handler(tauri::generate_handler![
            list_jobs, jobs_dir, compare_job, preflight, apply_job, cancel_run, pause_run,
            open_progress_window, close_progress_window, post_sync_action,
            run_history, last_syncs, run_detail,
            get_job, save_job, delete_job
        ])
        .run(tauri::generate_context!())
        .expect("error while running SyncDash");
}
