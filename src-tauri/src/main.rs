//! SyncDash desktop shell (Tauri v2): IPC orchestration only; all sync logic lives in the syncdash core library.
//! - v0.9 M1: unified event stream — the engine emits ProgressEvent through `run::*_with(ctx)`,
//!   TauriSink throttles them and forwards them to the frontend as `run-progress` events (carrying run_id);
//!   the legacy `progress` event is synthesized by a shim from PhaseStart/Progress, removed once the M2 frontend lands.
//! - Single-run mutual exclusion: RunState.active holds a RunCtl; cancel_run / pause_run act on it.
//! - reverse_op is precomputed for every op (zero logic drift for the frontend's "click a badge to flip direction")
//! - apply_job receives the frontend's final op list (already flipped and checked); the heavy work all goes through spawn_blocking

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use syncdash::compare::{self, Action, Op, Plan, PlanHeader};
use syncdash::progress::{Phase, ProgressEvent, ProgressSink, RunCtl, RunCtx};
use syncdash::{config, run};
use tauri::Emitter;

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
struct JobDto {
    name: String,
    mode: String,
    rigor: String,
    source: String,
    target: String,
    has_archive: bool,
    // v0.9 M3: fill in the fields the frontend needs to see (remote badge / versioning marker / filter hints)
    remote: bool,
    remote_host: Option<String>,
    versioning: bool,
    delta: bool,
    #[ts(type = "number | null")]
    parallel: Option<usize>,
    include: Vec<String>,
    exclude: Vec<String>,
    #[ts(type = "number | null")]
    watch_interval_secs: Option<u64>,
    watch_auto_apply: bool,
    /// 1:N: the effective target list (a single-target job = one entry). When >1 the frontend shows the target selector
    targets: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
struct PlanDto {
    header: PlanHeader,
    ops: Vec<Op>,
    /// One entry per op: rows that can be flipped carry the reverse op, the rest are null
    reversed: Vec<Option<Op>>,
    /// One entry per op: the size/mtime measured on both sides at compare time (for the table columns and sorting).
    /// A parallel array rather than extra fields on Op — Op literals appear in thirty-odd places in compare.rs,
    /// and that would change the on-disk plan JSONL format. The op shape preflight/apply receive stays unchanged.
    #[serde(default)]
    metas: Vec<compare::RowMeta>,
    /// Count/bytes of the files judged equal on both sides (the denominator of "showing X of Y")
    #[serde(default)]
    #[ts(type = "number")]
    equal_count: u64,
    #[serde(default)]
    #[ts(type = "number")]
    equal_bytes: u64,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
struct ApplyDto {
    #[ts(type = "number")]
    done: u64,
    #[ts(type = "number")]
    skipped: u64,
    #[ts(type = "number")]
    errors: u64,
    #[ts(type = "number")]
    bytes_copied: u64,
    cancelled: bool,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
struct PreflightDto {
    ok: bool,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Serialize, Default, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
struct PathInfo {
    exists: bool,
    is_dir: bool,
    has_marker: bool,
}

#[derive(Serialize, Default, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
struct PathVerdict {
    source: PathInfo,
    target: PathInfo,
    /// Plain-language warnings; the editor renders them right under the field
    warnings: Vec<String>,
}

// Snapshot cache (the data source for the "Identical" panel)
//
// compare already walked both sides in full; dropping the snapshots would force the UI to rescan just to glance at the identical items.
// Single-slot cache: every compare overwrites it and switching jobs invalidates it; two snapshots at the 20k-entry scale run to a dozen-odd MB.

struct CachedSnaps {
    job: String,
    source: syncdash::table::Snapshot,
    target: syncdash::table::Snapshot,
}

#[derive(Default)]
struct SnapCache(Mutex<Option<CachedSnaps>>);

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
struct SamePage {
    #[ts(type = "number")]
    total: u64,
    rows: Vec<compare::SameRow>,
    /// Which job's snapshot is sitting in the cache (on a mismatch the UI prompts for a fresh compare)
    job: String,
}

// Run state (mutual exclusion + cancel/pause handles)

#[derive(Default)]
struct RunState {
    active: Mutex<Option<Arc<RunCtl>>>,
    seq: AtomicU64,
}

fn begin_run(st: &RunState) -> Result<(u64, Arc<RunCtl>), String> {
    let mut g = st.active.lock().unwrap();
    if g.is_some() {
        return Err("Another run is already in progress — cancel it or wait for it to finish".into());
    }
    let ctl = RunCtl::new();
    *g = Some(ctl.clone());
    Ok((st.seq.fetch_add(1, Ordering::Relaxed) + 1, ctl))
}

fn end_run(st: &RunState) {
    *st.active.lock().unwrap() = None;
}

// Event bridge

/// The run-progress payload sent to the frontend: run_id lets the frontend drop late events from a cancelled run;
/// purpose separates compare from apply — the sub-window only accepts apply (otherwise the automatic re-check compare
/// after a sync would hijack the open result window into a forever-spinning "comparing"), the main panel only accepts compare.
#[derive(Serialize, Clone)]
struct RunEvent {
    run_id: u64,
    purpose: &'static str,
    #[serde(flatten)]
    ev: ProgressEvent,
}

/// Legacy status-bar event (a transitional shim until the M2 frontend lands)
#[derive(Serialize, Clone, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
struct LegacyProgress {
    phase: String,
    detail: String,
    /// Completion percentage of the hash/copy phase (0-100); boundary events use -1
    pct: i32,
    /// Reserved field: in the new stream the rate is computed by the frontend (4s sliding window), the shim no longer fakes it
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
                        syncdash::foundation::fmt::human_bytes(*bytes_done),
                        syncdash::foundation::fmt::human_bytes(*bytes_total)
                    ),
                    pct,
                    rate: 0.0,
                },
            );
        }
        _ => {}
    }
}

/// ProgressSink → Tauri events. Progress events are throttled to ≥100ms apiece (= the FFS chart sampling rate),
/// PhaseStart/Totals/Error/Paused/Resumed/Summary pass straight through.
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

/// 1:N: resolve a multi-target job into "the single-job view of the currently selected target" (the engine's single pipeline is reused as-is)
fn resolve_target(job: &config::Job, target_index: Option<usize>) -> Result<config::Job, String> {
    job.validate_multi_target()?;
    let list = job.target_list();
    let idx = target_index.unwrap_or(0);
    let t = list.get(idx).ok_or_else(|| format!("target index {idx} is out of range ({} total)", list.len()))?;
    Ok(job.for_target(t))
}

fn user_err(e: std::io::Error) -> String {
    if syncdash::progress::is_cancelled(&e) { "cancelled".into() } else { e.to_string() }
}

// Commands

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

/// M5: the editor reads the full Job (the list_jobs DTO is only a summary)
#[tauri::command]
fn get_job(name: String) -> Result<config::Job, String> {
    config::load(&name).map(|(_, j)| j).map_err(|e| e.to_string())
}

/// M5: save a job (create, or overwrite the TOML of the same name)
#[tauri::command]
fn save_job(name: String, job: config::Job) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("Job name cannot be empty".into());
    }
    config::save_job(name.trim(), &job).map(|p| p.display().to_string()).map_err(|e| e.to_string())
}

/// M5: delete the job's config file (not a single byte of data is touched)
#[tauri::command]
fn delete_job(name: String) -> Result<(), String> {
    config::delete_job(&name).map_err(|e| e.to_string())
}

// P1: path health check and "show in file explorer"

/// Normalize to a comparable form: lowercase + '/' separators + no trailing separator.
/// Used only to decide "are the two roots the same / nested"; it never affects sync semantics.
fn norm_root(p: &str) -> String {
    let s = p.trim().replace('\\', "/").to_lowercase();
    let s = s.trim_end_matches('/');
    s.to_string()
}

/// Live health check for the editor: whether the path exists, whether it is a directory, whether it
/// carries a mount-point marker, and how the two roots relate (identical / nested). A mistyped path costs
/// too much to be reported only in the status bar once Compare runs.
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
        v.warnings.push(format!("source does not exist: {s}"));
    } else if !s.is_empty() && !v.source.is_dir {
        v.warnings.push("source is not a directory".into());
    }
    if !t.is_empty() && !v.target.exists {
        v.warnings.push(format!("target does not exist: {t} (it will be created on the first sync)"));
    } else if !t.is_empty() && !v.target.is_dir {
        v.warnings.push("target is not a directory".into());
    }
    let (ns, nt) = (norm_root(s), norm_root(t));
    if !ns.is_empty() && ns == nt {
        v.warnings.push("source and target are the same directory".into());
    } else if !ns.is_empty() && !nt.is_empty() {
        // Nesting: mirror pours the outer contents into the inner root, then treats what it poured in as new outer content — it eats its own tail
        if nt.starts_with(&format!("{ns}/")) {
            v.warnings.push("target sits under source — nested roots copy into themselves".into());
        } else if ns.starts_with(&format!("{nt}/")) {
            v.warnings.push("source sits under target — nested roots copy into themselves".into());
        }
    }
    v
}

/// Ad-hoc mask matching for the UI funnel. The frontend **does not write its own glob** — the FFS mask
/// semantics have exactly one implementation, in filter.rs, so a mask tried out in the UI behaves identically once written into the job's exclude.
#[tauri::command]
fn mask_match(masks: Vec<String>, paths: Vec<String>) -> Vec<bool> {
    syncdash::filter::mask_hits(&masks, &paths)
}

/// Pagination for the "Identical" panel. The data source is the snapshot left by the last compare — no rescan.
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
        return Err("No compare results yet — run Compare first".into());
    };
    if c.job != name {
        return Err(format!("The cache holds the snapshot for '{}'; run Compare again for '{}'", c.job, name));
    }
    let (_n, job) = config::load(&name).map_err(|e| e.to_string())?;
    let (total, rows) = compare::same_page(&c.source, &c.target, &job.compare_opts(), &query, offset, limit.min(2000));
    Ok(SamePage { total, rows, job: c.job.clone() })
}

/// Export the current view as CSV. Escaping happens exactly once, here, and the output is UTF-8 **with a BOM** —
/// without the BOM Excel interprets the file in the local code page and non-ASCII paths turn into mojibake.
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
        // The local timezone offset is the UI's job; this writes ISO UTC so cross-machine reconciliation is unambiguous
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
    // Action/side use serde's snake_case form (see json_token), matching the literals in the plan JSONL
    // and the event stream — Debug's PascalCase would leave the CSV out of step with every other output
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

/// The public literals for enums follow serde (Action/Side are both marked rename_all = "snake_case"),
/// so delete_dir in the CSV is the same word the plan JSONL and the event stream write.
fn json_token<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default().trim_matches('"').to_string()
}

/// days → (y, m, d), Howard Hinnant's civil_from_days (dependency-free)
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

/// Select the path in the system file manager. Arguments go straight to the exe, not through a shell.
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("Path no longer exists: {path}"));
    }
    #[cfg(windows)]
    {
        // explorer returns exit 1 even when the selection succeeds, so the status code is meaningless here — all that matters is whether the process started
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

/// M4: run history (newest → oldest). job = null shows everything
#[tauri::command]
fn run_history(job: Option<String>, limit: Option<usize>) -> Vec<syncdash::runlog::RunRecord> {
    syncdash::runlog::history(job.as_deref(), limit.unwrap_or(50))
}

/// M4: the most recent run per job — the data behind the sidebar's "last sync" dot
#[tauri::command]
fn last_syncs() -> std::collections::HashMap<String, syncdash::runlog::RunRecord> {
    syncdash::runlog::latest_by_job()
}

/// M4: the detail lines of one run (raw JSONL; line count capped)
#[tauri::command]
fn run_detail(detail: String) -> Vec<String> {
    syncdash::runlog::detail_lines(&detail, 2000)
}

// v0.10: centralized logging and app settings

/// The run list. Unlike `run_history`, this one also folds in **interrupted runs** (the ones missing
/// from the index that left only a directory behind) — the crashed run is exactly the one worth seeing.
#[tauri::command]
fn log_runs(job: Option<String>, limit: Option<usize>) -> Vec<syncdash::runlog::RunRecord> {
    syncdash::runlog::history_merged(job.as_deref(), limit.unwrap_or(100))
}

/// One artifact of a run (which ∈ run / errors / items / plan / summary).
/// Line count capped: the apply manifest records everything, one large sync runs to tens of thousands of lines, and shipping all of it over IPC would freeze the UI.
#[tauri::command]
fn log_artifact(run_id: String, which: String, max: Option<usize>) -> Vec<String> {
    syncdash::runlog::artifact_lines(&run_id, &which, max.unwrap_or(5000))
}

/// The log root directory (the "open folder" button hands it to the existing `reveal`)
#[tauri::command]
fn log_dir_path(run_id: Option<String>) -> String {
    let root = syncdash::runlog::logs_dir();
    match run_id {
        Some(id) if !id.is_empty() => root.join(id).display().to_string(),
        _ => root.display().to_string(),
    }
}

/// Events outside of a run (startup, settings errors, prune, migration). Returns the last n lines.
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

/// Save settings. `migrate` = move the whole old directory over when the log directory changes.
///
/// The old location must be resolved **before** the new config is written — ask afterwards and you only get the new value.
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

/// Open (or focus) the standalone progress sub-window (Synchronize only; compare progress is shown inline in the main window).
/// **Must be an async command**: sync commands run on the main thread's IPC, while wry needs the main event loop
/// to pump messages in order to create a window — creating it synchronously leaves the sub-window's navigation stuck
/// on about:blank (an all-white window) and close events never get queued (it looks like "the whole app won't close").
/// An async command runs on its own thread, so window creation is proxied through the event loop correctly.
#[tauri::command]
async fn open_progress_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("progress") {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(&app, "progress", tauri::WebviewUrl::App("progress.html".into()))
        .title("SyncDash — Run")
        .inner_size(620.0, 500.0)
        .min_inner_size(440.0, 380.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Destroy the progress sub-window. Not hide: a hidden sub-window keeps the process alive after the main window closes
#[tauri::command]
async fn close_progress_window(app: tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("progress") {
        let _ = w.destroy();
    }
}

/// When-finished action (same as FFS): sleep / shutdown. The countdown and confirmation both happen in the frontend before this is called.
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

/// Request cooperative cancellation of the active run. Returns whether an active run existed.
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

/// Pause/resume the active run (elapsed stops growing while paused, the RootLock heartbeat keeps beating)
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
        // Take over the process-level log outlet during compare too: the diagnostics in `trash`/`lock`/`scan`
        // that cannot reach a ctx go through the macro registry, and without installing here they fall back to stderr — in a windowed build that means never said.
        let _log_guard = syncdash::progress::install(ctx.sink.clone());
        let t0 = std::time::Instant::now();
        let ts_ms = syncdash::foundation::time::now_ms() as i64;
        // M3: remote jobs take the remote pipeline (scanning on the remote's own disk) instead of silently falling into the local one
        let r = if job.remote_host.is_some() {
            run::compare_remote_job_detailed(&name, &job, &ctx)
        } else {
            run::compare_job_detailed(&job, &ctx)
        };
        end_run(&st);
        // compare has no side effects: one index line, no directory. A 30s watch cycle = 2880 runs a day,
        // and creating a directory each time would flood the log disk.
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
        // Evidence layer: measured size/mtime on both sides + equal-item counts. It shares the same
        // norm_key/files_equal as compare(), so the definitions cannot drift apart.
        let ev = compare::evidence(&out.source, &out.target, &out.plan, &job.compare_opts());
        let dto = PlanDto {
            header: out.plan.header,
            ops: out.plan.ops,
            reversed,
            metas: ev.metas,
            equal_count: ev.equal_count,
            equal_bytes: ev.equal_bytes,
        };
        // Snapshots are kept for the "Identical" panel; single slot, overwritten by the next compare
        *cache.0.lock().unwrap() = Some(CachedSnaps { job: name, source: out.source, target: out.target });
        Ok(dto)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Pre-sync gate checks (disk space / deletion ratio). The frontend shows the result in the confirmation sheet,
/// so "why won't it let me sync" has somewhere to be answered instead of only appearing on stderr.
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
        // remote jobs only get the deletion-ratio gate (disk space/marker live on the remote machine, so checking locally would be wrong)
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
        // Touch nothing when a gate fails, and hand the reason back to the UI
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
        // M4: every real apply writes a run-log entry (the Recorder also collects error events into the detail file)
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
    // A windowed build has no console — the only home for diagnostics outside a run (settings parse
    // failures, pruning, migration) is app.jsonl. `_log` must live until the process exits: writing
    // `let _ = …` drops it on the spot and the sink is unhooked immediately.
    let cfg = syncdash::settings::load();
    let _log = std::sync::Arc::new(syncdash::logging::AppLogSink::open(&cfg.resolved_log_dir(), cfg.level));
    let _log_guard = syncdash::progress::install(_log.clone());
    // Retention runs once at startup: the apply manifest records everything and grows without a gate
    let dropped = syncdash::runlog::prune(cfg.keep_days, cfg.max_total_mb);
    if dropped > 0 {
        syncdash::log_info!("app", "Log cleanup: removed the records of {dropped} runs");
    }
    tauri::Builder::default()
        // Main window closes → cascade-destroy the progress sub-window; a leftover window keeps Tauri from exiting ("the app won't close")
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
            source_excluded: 0, target_excluded: 0,
        };
        let ops = vec![Op {
            side: Side::Target,
            action: Action::DeleteDir,
            // Both a comma and a double quote in the path — the two classic CSV landmines
            path: "b/y,z\"q.txt".into(),
            from: None, size: Some(20), mtime_ms: None, hash: None, link: None, mode: None,
            reason: "gone, really".into(),
        }];
        // mtime_ms = 0 means "no time available" in a snapshot (scan writes 0 when it cannot read metadata),
        // so use real non-zero times here instead of pressing the epoch into service as a date
        let metas = vec![compare::RowMeta {
            src: Some(SideMeta { size: 10, mtime_ms: 86_400_000 }),
            dst: Some(SideMeta { size: 20, mtime_ms: 172_800_000 }),
        }];
        let n = export_csv(out.display().to_string(), header, ops, metas, vec![true]).unwrap();
        assert_eq!(n, 1);
        let bytes = std::fs::read(&out).unwrap();
        // Excel does not recognize UTF-8 without a BOM; non-ASCII paths turn the whole column into mojibake
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
        let row = text.lines().nth(1).unwrap();
        assert!(row.contains("\"b/y,z\"\"q.txt\""), "commas and quotes in the path must be escaped per RFC4180: {row}");
        assert!(row.contains("\"gone, really\""));
        // Enum literals share their source with the plan JSONL (snake_case), not Debug's PascalCase
        assert!(row.contains(",delete_dir,target,"), "enums should be snake_case: {row}");
        // Both sides' size/time must be written out
        assert!(row.contains(",10,1970-01-02T00:00:00Z,20,1970-01-03T00:00:00Z,"), "{row}");
        // The absent side leaves empty columns — no zero fill, no fabricated date
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
            source_excluded: 0, target_excluded: 0,
        };
        export_csv(out.display().to_string(), h2, ops2, one_sided, vec![false]).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let row = text.lines().nth(1).unwrap();
        assert!(row.starts_with("0,delete,target,x,,/s/x,/t/x,,,5,1970-01-02T00:00:00Z,gone"), "{row}");
        let _ = std::fs::remove_file(&out);
    }
}
