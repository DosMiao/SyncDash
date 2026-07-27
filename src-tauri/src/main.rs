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
    /// 1:N：生效的 target 列表（单 target 任务 = 一项）。>1 时前端显示 target 选择器
    targets: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct PlanDto {
    header: PlanHeader,
    ops: Vec<Op>,
    /// 与 ops 一一对应：可翻方向的行给出反向 op，不可翻为 null
    reversed: Vec<Option<Op>>,
    /// 与 ops 一一对应：两侧比对时点的实测 size/mtime（界面列与排序用）。
    /// 走平行数组而不是往 Op 里加字段——Op 的字面量在 compare.rs 里有三十多处，
    /// 且那会改变 plan JSONL 的落盘格式。preflight/apply 收到的 ops 形状不变。
    #[serde(default)]
    metas: Vec<compare::RowMeta>,
    /// 两侧判定相等的文件数/字节（"显示 X / 共 Y"的分母）
    #[serde(default)]
    equal_count: u64,
    #[serde(default)]
    equal_bytes: u64,
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

#[derive(Serialize, Default)]
struct PathInfo {
    exists: bool,
    is_dir: bool,
    has_marker: bool,
}

#[derive(Serialize, Default)]
struct PathVerdict {
    source: PathInfo,
    target: PathInfo,
    /// 人话警告，编辑器直接贴在字段下面
    warnings: Vec<String>,
}

// ---------- 快照缓存（"相同项"面板的数据源） ----------
//
// compare 已经把两侧完整扫过一遍，丢掉快照等于逼界面为了看一眼"相同项"再扫一次。
// 单槽缓存：每次 compare 覆盖，换任务即失效；两份快照 2 万条目量级约十几 MB。

struct CachedSnaps {
    job: String,
    source: syncdash::table::Snapshot,
    target: syncdash::table::Snapshot,
}

#[derive(Default)]
struct SnapCache(Mutex<Option<CachedSnaps>>);

#[derive(Serialize)]
struct SamePage {
    total: u64,
    rows: Vec<compare::SameRow>,
    /// 缓存里躺的是哪个任务的快照（对不上就让界面提示重新比对）
    job: String,
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

/// 1:N：把多 target 任务解析成"当前选中 target 的单任务视角"（引擎单管线原样复用）
fn resolve_target(job: &config::Job, target_index: Option<usize>) -> Result<config::Job, String> {
    job.validate_multi_target()?;
    let list = job.target_list();
    let idx = target_index.unwrap_or(0);
    let t = list.get(idx).ok_or_else(|| format!("target 序号 {idx} 越界（共 {} 个）", list.len()))?;
    Ok(job.for_target(t))
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
            targets: j.target_list().iter().map(|t| t.display().to_string()).collect(),
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

// ---------- P1：路径体检与"在资源管理器中显示" ----------

/// 归一化到可比较形态：小写 + 统一 '/' + 去尾分隔符。
/// 只用于"两根是否相同/是否嵌套"的判断，不参与任何同步语义。
fn norm_root(p: &str) -> String {
    let s = p.trim().replace('\\', "/").to_lowercase();
    let s = s.trim_end_matches('/');
    s.to_string()
}

/// 编辑器实时体检：路径存不存在、是不是目录、有没有挂载点标记，
/// 以及两根之间的关系（相同 / 互相嵌套）。写错路径的代价太大，
/// 不该等到 Compare 才在状态栏里说。
#[tauri::command]
fn inspect_paths(source: String, target: String) -> PathVerdict {
    fn info(p: &str) -> PathInfo {
        if p.trim().is_empty() {
            return PathInfo::default();
        }
        let path = std::path::Path::new(p.trim());
        let is_dir = path.is_dir();
        PathInfo {
            exists: is_dir || path.is_file(),
            is_dir,
            has_marker: is_dir && syncdash::preflight::has_marker(path),
        }
    }
    let mut v = PathVerdict { source: info(&source), target: info(&target), warnings: Vec::new() };
    let (s, t) = (source.trim(), target.trim());
    if !s.is_empty() && !v.source.exists {
        v.warnings.push(format!("source 不存在：{s}"));
    } else if !s.is_empty() && !v.source.is_dir {
        v.warnings.push("source 不是目录".into());
    }
    if !t.is_empty() && !v.target.exists {
        v.warnings.push(format!("target 不存在：{t}（首次同步会自动创建）"));
    } else if !t.is_empty() && !v.target.is_dir {
        v.warnings.push("target 不是目录".into());
    }
    let (ns, nt) = (norm_root(s), norm_root(t));
    if !ns.is_empty() && ns == nt {
        v.warnings.push("source 与 target 是同一个目录".into());
    } else if !ns.is_empty() && !nt.is_empty() {
        // 嵌套：mirror 会把外层的内容往内层灌，再把灌进去的当成外层新增——自食其尾
        if nt.starts_with(&format!("{ns}/")) {
            v.warnings.push("target 在 source 之下——嵌套的两根会自我复制".into());
        } else if ns.starts_with(&format!("{nt}/")) {
            v.warnings.push("source 在 target 之下——嵌套的两根会自我复制".into());
        }
    }
    v
}

/// 界面漏斗的即席掩码匹配。前端**不自己写 glob**——同一套 FFS 掩码语义
/// 只有 filter.rs 一份实现，界面里试出来的掩码写进任务 exclude 后行为一致。
#[tauri::command]
fn mask_match(masks: Vec<String>, paths: Vec<String>) -> Vec<bool> {
    syncdash::filter::mask_hits(&masks, &paths)
}

/// "相同项"分页。数据源是上一次 compare 留下的快照——不重扫。
#[tauri::command]
fn list_same(
    snaps: tauri::State<'_, Arc<SnapCache>>,
    name: String,
    query: String,
    offset: usize,
    limit: usize,
) -> Result<SamePage, String> {
    let g = snaps.0.lock().unwrap();
    let Some(c) = g.as_ref() else {
        return Err("还没有比对结果——先 Compare".into());
    };
    if c.job != name {
        return Err(format!("缓存里是 '{}' 的快照，请对 '{}' 重新 Compare", c.job, name));
    }
    let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
    let (total, rows) = compare::same_page(&c.source, &c.target, &job.compare_opts(), &query, offset, limit.min(2000));
    Ok(SamePage { total, rows, job: c.job.clone() })
}

/// 导出当前视图为 CSV。转义只在这里做一次，UTF-8 **带 BOM**——
/// 不加 BOM 的话 Excel 会按本地代码页解释，中文路径直接乱码。
#[tauri::command]
fn export_csv(
    path: String,
    header: PlanHeader,
    ops: Vec<Op>,
    metas: Vec<compare::RowMeta>,
    checked: Vec<bool>,
) -> Result<usize, String> {
    use std::io::Write;
    fn esc(s: &str) -> String {
        if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    }
    fn stamp(ms: i64) -> String {
        if ms <= 0 {
            return String::new();
        }
        // 本地时区偏移交给界面；这里落 UTC 的 ISO 形态，跨机对账不会歧义
        let secs = ms / 1000;
        let days = secs.div_euclid(86_400);
        let tod = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", tod / 3600, (tod % 3600) / 60, tod % 60)
    }
    let f = std::fs::File::create(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut w = std::io::BufWriter::new(f);
    w.write_all(&[0xEF, 0xBB, 0xBF]).map_err(|e| e.to_string())?;
    writeln!(w, "checked,action,side,rel_path,from,source_path,target_path,src_size,src_mtime_utc,dst_size,dst_mtime_utc,reason")
        .map_err(|e| e.to_string())?;
    let sep = if header.target_root.contains('\\') { '\\' } else { '/' };
    let join = |root: &str, rel: &str| {
        let r = root.trim_end_matches(['/', '\\']);
        let rel = if sep == '\\' { rel.replace('/', "\\") } else { rel.to_string() };
        format!("{r}{sep}{rel}")
    };
    // 动作/侧别用 serde 的 snake_case 形态（见 json_token），与 plan JSONL、
    // 事件流的字面量一致——Debug 的 PascalCase 会让 CSV 和其它出口对不上账
    for (i, op) in ops.iter().enumerate() {
        let m = metas.get(i).cloned().unwrap_or_default();
        let on = checked.get(i).copied().unwrap_or(false);
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            if on { 1 } else { 0 },
            json_token(&op.action),
            json_token(&op.side),
            esc(&op.path),
            esc(op.from.as_deref().unwrap_or("")),
            esc(&join(&header.source_root, &op.path)),
            esc(&join(&header.target_root, &op.path)),
            m.src.map(|s| s.size.to_string()).unwrap_or_default(),
            m.src.map(|s| stamp(s.mtime_ms)).unwrap_or_default(),
            m.dst.map(|s| s.size.to_string()).unwrap_or_default(),
            m.dst.map(|s| stamp(s.mtime_ms)).unwrap_or_default(),
            esc(&op.reason),
        )
        .map_err(|e| e.to_string())?;
    }
    w.flush().map_err(|e| e.to_string())?;
    Ok(ops.len())
}

/// 枚举的对外字面量以 serde 为准（Action/Side 都标了 rename_all = "snake_case"），
/// 这样 CSV 里的 delete_dir 与 plan JSONL、事件流写的是同一个词。
fn json_token<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default().trim_matches('"').to_string()
}

/// days → (y, m, d)，Howard Hinnant 的 civil_from_days（无依赖）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 在系统文件管理器里选中该路径。参数单独传给 exe，不过 shell。
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("路径已不存在：{path}"));
    }
    #[cfg(windows)]
    {
        // explorer 选中成功时也返回 exit 1，状态码在这里没有意义——只看能不能起进程
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg("-R").arg(p).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if p.is_dir() { p } else { p.parent().unwrap_or(p) };
        std::process::Command::new("xdg-open").arg(dir).spawn().map_err(|e| e.to_string())?;
    }
    Ok(())
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

// ---------- v0.10：集中日志与 app 设置 ----------

/// 运行列表。与 `run_history` 的区别：这条会把**被中断的运行**（索引里没有、
/// 只剩目录的那些）一并补进来——崩溃那次恰恰是最该被看见的一次。
#[tauri::command]
fn log_runs(job: Option<String>, limit: Option<usize>) -> Vec<syncdash::runlog::RunRecord> {
    syncdash::runlog::history_merged(job.as_deref(), limit.unwrap_or(100))
}

/// 一次运行的某份产物（which ∈ run / errors / items / plan / summary）。
/// 行数封顶：执行清单全记，一次大同步上万行，整份塞进 IPC 会把界面卡死。
#[tauri::command]
fn log_artifact(run_id: String, which: String, max: Option<usize>) -> Vec<String> {
    syncdash::runlog::artifact_lines(&run_id, &which, max.unwrap_or(5000))
}

/// 日志根目录（"打开目录"按钮把它交给已有的 `reveal`）
#[tauri::command]
fn log_dir_path(run_id: Option<String>) -> String {
    let root = syncdash::runlog::logs_dir();
    match run_id {
        Some(id) if !id.is_empty() => root.join(id).display().to_string(),
        _ => root.display().to_string(),
    }
}

/// 运行之外的事件（启动、设置错误、prune、迁移）。返回最后 n 行。
#[tauri::command]
fn app_log_tail(n: Option<usize>) -> Vec<String> {
    let n = n.unwrap_or(500);
    let p = syncdash::runlog::logs_dir().join(syncdash::logging::AppLogSink::FILE);
    let Ok(text) = std::fs::read_to_string(p) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines.iter().rev().take(n).rev().map(|s| s.to_string()).collect()
}

#[tauri::command]
fn get_settings() -> syncdash::settings::AppSettings {
    syncdash::settings::load()
}

/// 保存设置。`migrate` = 日志目录变了时把旧目录整体搬过去。
///
/// 迁移要在**写新配置之前**算好旧位置——写完再问就只能问到新值了。
#[tauri::command]
fn save_settings(
    s: syncdash::settings::AppSettings,
    migrate: bool,
) -> Result<syncdash::settings::MigrateReport, String> {
    let old_dir = syncdash::settings::load().wanted_log_dir();
    let new_dir = s.wanted_log_dir();
    syncdash::settings::save(&s).map_err(|e| e.to_string())?;
    if migrate && old_dir != new_dir {
        Ok(syncdash::settings::migrate_log_dir(&old_dir, &new_dir))
    } else {
        Ok(syncdash::settings::MigrateReport::default())
    }
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
    snaps: tauri::State<'_, Arc<SnapCache>>,
    name: String,
    target_index: Option<usize>,
) -> Result<PlanDto, String> {
    let st = state.inner().clone();
    let cache = snaps.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        let job = resolve_target(&job, target_index)?;
        let (run_id, ctl) = begin_run(&st)?;
        let ctx = make_ctx(&app, run_id, ctl, "compare");
        // 比对期也接管进程级日志出口：`trash`/`lock`/`scan` 里那些拿不到 ctx 的诊断
        // 走宏经注册表，这里不装的话它们会退回 stderr——windowed 构建里等于没说。
        let _log_guard = syncdash::logging::install(ctx.sink.clone());
        let t0 = std::time::Instant::now();
        let ts_ms = syncdash::table::now_ms() as i64;
        // M3：remote 任务走远程管线（远端自己盘上扫描），不再静默落进本地管线
        let r = if job.remote_host.is_some() {
            run::compare_remote_job_detailed(&name, &job, &ctx)
        } else {
            run::compare_job_detailed(&job, &ctx)
        };
        end_run(&st);
        // compare 无副作用：只留一行索引，不建目录。watch 30s 一轮 = 一天 2880 次，
        // 每次建目录会把日志盘冲垮。
        syncdash::runlog::compare_summary(
            &name,
            if job.remote_host.is_some() { "remote-compare" } else { "compare" },
            ts_ms,
            r.as_ref().map(|o| o.plan.ops.len() as u64).unwrap_or(0),
            t0.elapsed().as_millis() as u64,
            r.as_ref().err().map(syncdash::progress::is_cancelled).unwrap_or(false),
        );
        let out = r.map_err(user_err)?;
        let reversed = out.plan.ops.iter().map(compare::reverse_op).collect();
        // 证据层：两侧实测 size/mtime + 相等项统计。与 compare() 共用同一套
        // norm_key/files_equal，口径不会漂移。
        let ev = compare::evidence(&out.source, &out.target, &out.plan, &job.compare_opts());
        let dto = PlanDto {
            header: out.plan.header,
            ops: out.plan.ops,
            reversed,
            metas: ev.metas,
            equal_count: ev.equal_count,
            equal_bytes: ev.equal_bytes,
        };
        // 快照留给"相同项"面板；单槽，下次 compare 直接覆盖
        *cache.0.lock().unwrap() = Some(CachedSnaps { job: name, source: out.source, target: out.target });
        Ok(dto)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 同步前的闸门体检（磁盘空间 / 删除占比）。前端在确认单里展示结果，
/// 让"为什么不让我同步"这句话有地方说，而不是只出现在 stderr。
#[tauri::command]
async fn preflight(name: String, plan: PlanDto, ops: Vec<Op>, acknowledged: bool, target_index: Option<usize>) -> Result<PreflightDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        let job = resolve_target(&job, target_index)?;
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
    target_index: Option<usize>,
) -> Result<ApplyDto, String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
        let job = resolve_target(&job, target_index)?;
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
        let rec =
            syncdash::runlog::Recorder::start(&name, if remote { "remote-apply" } else { "apply" }, &ctx, &ops);
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
        rec.finish(&out, t0.elapsed().as_millis() as u64);
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
    syncdash::scan::init_worker_pool();
    // windowed 构建没有控制台 —— 运行之外的诊断（设置解析失败、清理、迁移）
    // 唯一的去处就是 app.jsonl。`_log` 必须活到进程结束：写成 `let _ = …`
    // 会当场 drop，sink 立刻被摘掉。
    let cfg = syncdash::settings::load();
    let _log = std::sync::Arc::new(syncdash::logging::AppLogSink::open(&cfg.resolved_log_dir(), cfg.level));
    let _log_guard = syncdash::logging::install(_log.clone());
    // 保留策略在启动时跑一次：执行清单是全记的，没有闸门会一直涨
    let dropped = syncdash::runlog::prune(cfg.keep_days, cfg.max_total_mb);
    if dropped > 0 {
        syncdash::log_info!("app", "日志清理：移除 {dropped} 次运行的记录");
    }
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
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(RunState::default()))
        .manage(Arc::new(SnapCache::default()))
        .invoke_handler(tauri::generate_handler![
            list_jobs, jobs_dir, compare_job, preflight, apply_job, cancel_run, pause_run,
            open_progress_window, close_progress_window, post_sync_action,
            run_history, last_syncs, run_detail,
            get_job, save_job, delete_job,
            inspect_paths, reveal, mask_match, list_same, export_csv,
            log_runs, log_artifact, log_dir_path, app_log_tail, get_settings, save_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running SyncDash");
}

#[cfg(test)]
mod tests {
    use super::*;
    use syncdash::compare::{Action, Side, SideMeta};

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn csv_escapes_commas_and_quotes_and_carries_both_sides() {
        let dir = std::env::temp_dir().join("syncdash-csv-test");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("plan.csv");
        let header = PlanHeader {
            schema: 1, kind: "plan".into(), mode: "mirror".into(), generated_at_ms: 0,
            source_root: r"D:\S".into(), source_host: "h".into(),
            target_root: r"E:\T".into(), target_host: "h".into(),
            op_count: 1, conflict_count: 0, source_entries: 1, target_entries: 1,
        };
        let ops = vec![Op {
            side: Side::Target,
            action: Action::DeleteDir,
            // 逗号与双引号都在路径里——CSV 的两个经典地雷
            path: "b/y,z\"q.txt".into(),
            from: None, size: Some(20), mtime_ms: None, hash: None, link: None, mode: None,
            reason: "gone, really".into(),
        }];
        // mtime_ms = 0 在快照里的含义是"取不到时间"（scan 拿不到 metadata 时写 0），
        // 所以这里用真实的非零时间，别拿纪元当日期使
        let metas = vec![compare::RowMeta {
            src: Some(SideMeta { size: 10, mtime_ms: 86_400_000 }),
            dst: Some(SideMeta { size: 20, mtime_ms: 172_800_000 }),
        }];
        let n = export_csv(out.display().to_string(), header, ops, metas, vec![true]).unwrap();
        assert_eq!(n, 1);
        let bytes = std::fs::read(&out).unwrap();
        // Excel 不认没有 BOM 的 UTF-8，中文路径会整列乱码
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
        let row = text.lines().nth(1).unwrap();
        assert!(row.contains("\"b/y,z\"\"q.txt\""), "路径里的逗号与引号要按 RFC4180 转义: {row}");
        assert!(row.contains("\"gone, really\""));
        // 枚举字面量与 plan JSONL 同源（snake_case），不是 Debug 的 PascalCase
        assert!(row.contains(",delete_dir,target,"), "枚举应为 snake_case: {row}");
        // 双侧 size/时间都要落地
        assert!(row.contains(",10,1970-01-02T00:00:00Z,20,1970-01-03T00:00:00Z,"), "{row}");
        // 缺席的一侧留空列，不补 0 也不伪造日期
        let one_sided = vec![compare::RowMeta { src: None, dst: Some(SideMeta { size: 5, mtime_ms: 86_400_000 }) }];
        let ops2 = vec![Op {
            side: Side::Target, action: Action::Delete, path: "x".into(), from: None,
            size: Some(5), mtime_ms: None, hash: None, link: None, mode: None, reason: "gone".into(),
        }];
        let h2 = PlanHeader {
            schema: 1, kind: "plan".into(), mode: "mirror".into(), generated_at_ms: 0,
            source_root: "/s".into(), source_host: "h".into(),
            target_root: "/t".into(), target_host: "h".into(),
            op_count: 1, conflict_count: 0, source_entries: 1, target_entries: 1,
        };
        export_csv(out.display().to_string(), h2, ops2, one_sided, vec![false]).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let row = text.lines().nth(1).unwrap();
        assert!(row.starts_with("0,delete,target,x,,/s/x,/t/x,,,5,1970-01-02T00:00:00Z,gone"), "{row}");
        let _ = std::fs::remove_file(&out);
    }
}
