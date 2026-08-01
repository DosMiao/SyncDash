//! Running a job: compare, preflight, apply, and the cancel/pause controls over a live run.
//!
//! The transport choice — this process or an ssh peer — is `run`'s, not this module's. These call
//! `run::compare` / `run::preflight` / `run::apply` and let the router decide.

use std::sync::Arc;

use syncdash::model::plan::{Action, Op, Plan};
use syncdash::pipeline::compare;
use syncdash::{job, run};
use tauri::Emitter;

use crate::bridge::make_ctx;
use crate::dto::{ApplyDto, PlanDto, PreflightDto};
use crate::state::{
    begin_run, begin_run_for_launch, end_run, release_progress_launch, resolve_target, user_err,
    CachedSnaps, RunState, SnapCache,
};

#[derive(Clone, serde::Serialize)]
struct RunRejected {
    launch_id: u64,
    message: String,
}

struct ActiveRunGuard(Arc<RunState>);

impl Drop for ActiveRunGuard {
    fn drop(&mut self) {
        end_run(&self.0);
    }
}

/// Request cooperative cancellation of the active run. Returns whether an active run existed.
#[tauri::command]
pub fn cancel_run(state: tauri::State<'_, Arc<RunState>>) -> bool {
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
pub fn pause_run(state: tauri::State<'_, Arc<RunState>>, paused: bool) -> bool {
    match state.active.lock().unwrap().as_ref() {
        Some(ctl) => {
            ctl.set_paused(paused);
            true
        }
        None => false,
    }
}

#[tauri::command]
pub async fn compare_job(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<RunState>>,
    snaps: tauri::State<'_, Arc<SnapCache>>,
    name: String,
    target_index: Option<usize>,
    accept_caps: Option<bool>,
) -> Result<PlanDto, String> {
    let st = state.inner().clone();
    let cache = snaps.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = job::load(&name).map_err(|e| e.to_string())?;
        let job = resolve_target(&job, target_index)?;
        let (run_id, ctl) = begin_run(&st)?;
        let _active_run = ActiveRunGuard(st.clone());
        let ctx = make_ctx(&app, run_id, ctl, "compare");
        // Take over the process-level log outlet during compare too: the diagnostics in `trash`/`lock`/`scan`
        // that cannot reach a ctx go through the macro registry, and without installing here they fall back to stderr — in a windowed build that means never said.
        //
        // **Layer, never replace.** `install` swaps the whole outlet, so handing it the Tauri sink
        // bare unhooks `AppLogSink` for exactly the window in which the scan reports what it could
        // not read — the run that most needs a durable record is the one that leaves none. Apply
        // already gets this right through `runlog::Recorder`; `progress::current`'s own doc comment
        // states the rule. A `None` here is a real absence (no outlet was installed at startup),
        // not a failure to paper over.
        let outlet: Arc<dyn syncdash::obs::progress::ProgressSink> =
            match syncdash::obs::progress::current() {
                Some(prev) => Arc::new(syncdash::obs::logging::MultiSink::new(vec![ctx.sink.clone(), prev])),
                None => ctx.sink.clone(),
            };
        let _log_guard = syncdash::obs::progress::install(outlet);
        let t0 = std::time::Instant::now();
        let ts_ms = syncdash::foundation::time::now_ms() as i64;
        // M3: remote jobs take the remote pipeline (scanning on the remote's own disk) instead of silently falling into the local one
        // A degraded run without consent refuses with the NeedsAck lines; the frontend shows
        // them and re-invokes with accept_caps=true if the user agrees.
        let r = run::compare(&name, &job, &ctx, accept_caps.unwrap_or(false));
        // compare has no side effects: one index line, no directory. A 30s watch cycle = 2880 runs a day,
        // and creating a directory each time would flood the log disk.
        syncdash::obs::runlog::compare_summary(
            &name,
            &run::run_kind(&job, "compare"),
            ts_ms,
            r.as_ref().map(|o| o.plan.ops.len() as u64).unwrap_or(0),
            t0.elapsed().as_millis() as u64,
            r.as_ref().err().map(syncdash::obs::progress::is_cancelled).unwrap_or(false),
        );
        let out = r.map_err(user_err)?;
        // Evidence layer: measured size/mtime on both sides + equal-item counts. It shares the same
        // norm_key/files_equal as compare(), so the definitions cannot drift apart.
        let ev = compare::evidence::evidence(&out.source, &out.target, &out.plan, &job.compare_opts());
        let metas = ev
            .metas
            .into_iter()
            .zip(&out.plan.ops)
            .map(|(meta, op)| {
                if matches!(op.action, Action::Copy) && op.size.is_some() && op.mtime_ms.is_some() {
                    None
                } else {
                    Some(meta)
                }
            })
            .collect();
        let dto = PlanDto {
            header: out.plan.header,
            ops: out.plan.ops,
            metas,
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
pub async fn preflight(name: String, plan: PlanDto, ops: Vec<Op>, acknowledged: bool, target_index: Option<usize>) -> Result<PreflightDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_n, job) = job::load(&name).map_err(|e| e.to_string())?;
        let job = resolve_target(&job, target_index)?;
        let full = Plan { header: plan.header.clone(), ops: plan.ops.clone() };
        let ops: Vec<Op> = ops
            .into_iter()
            .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
            .collect();
        // A peer job only gets the deletion-ratio gate: disk space and the marker live on the
        // remote machine, so checking them here would be answering about the wrong disk.
        let v = run::preflight(&job, &full, &ops, acknowledged);
        Ok(PreflightDto { ok: v.ok(), blockers: v.blockers, warnings: v.warnings })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn apply_job(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<RunState>>,
    name: String,
    plan: PlanDto,
    ops: Vec<Op>,
    acknowledged: bool,
    target_index: Option<usize>,
    accept_caps: Option<bool>,
    launch_id: Option<u64>,
) -> Result<ApplyDto, String> {
    let st = state.inner().clone();
    let reject_state = state.inner().clone();
    let requested_launch = launch_id;
    let run_app = app.clone();
    let joined = tauri::async_runtime::spawn_blocking(move || -> Result<ApplyDto, (String, bool)> {
        let (_n, job) = job::load(&name).map_err(|e| (e.to_string(), false))?;
        let job = resolve_target(&job, target_index).map_err(|e| (e, false))?;
        let full = Plan { header: plan.header.clone(), ops: plan.ops.clone() };
        let ops: Vec<Op> = ops
            .into_iter()
            .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
            .collect();
        // Touch nothing when a gate fails, and hand the reason back to the UI
        let v = run::preflight(&job, &full, &ops, acknowledged);
        if !v.ok() {
            return Err((v.blockers.join("\n"), false));
        }
        let (run_id, ctl) = begin_run_for_launch(&st, launch_id).map_err(|e| (e, false))?;
        let _active_run = ActiveRunGuard(st.clone());
        let ctx = make_ctx(&run_app, run_id, ctl, "apply");
        // M4: every real apply writes a run-log entry (the Recorder also collects error events into the detail file)
        let t0 = std::time::Instant::now();
        let rec = syncdash::obs::runlog::Recorder::start(&name, &run::run_kind(&job, "apply"), &ctx, &ops);
        // A degraded apply without consent refuses with the NeedsAck lines; the frontend shows
        // them and re-invokes with accept_caps=true if the user agrees.
        let out = match run::apply(
            &name, &job, &full, &ops, None, false, acknowledged, accept_caps.unwrap_or(false), &rec.ctx,
        ) {
            Ok(o) => o,
            Err(e) => return Err((user_err(e), true)),
        };
        rec.finish(&out, t0.elapsed().as_millis() as u64);
        Ok(ApplyDto {
            done: out.done,
            skipped: out.skipped,
            errors: out.errors,
            bytes_copied: out.bytes_copied,
            cancelled: out.cancelled,
        })
    })
    .await;
    let result = match joined {
        Ok(result) => result,
        Err(e) => {
            let message = e.to_string();
            if let Some(launch_id) = requested_launch {
                // A panic before begin leaves the reservation pending; a panic after begin has
                // consumed it (and ActiveRunGuard has already released the active slot). In both
                // cases the same launch needs a terminal signal or its window waits forever.
                release_progress_launch(&reject_state, launch_id);
                let _ = app.emit(
                    "run-rejected",
                    RunRejected { launch_id, message: message.clone() },
                );
            }
            return Err(message);
        }
    };
    match result {
        Ok(out) => Ok(out),
        Err((message, began)) => {
            if !began {
                if let Some(launch_id) = requested_launch {
                    if release_progress_launch(&reject_state, launch_id) {
                        let _ = app.emit(
                            "run-rejected",
                            RunRejected { launch_id, message: message.clone() },
                        );
                    }
                }
            }
            Err(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_run_guard_releases_state_during_unwind() {
        let state = Arc::new(RunState::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let state = state.clone();
            move || {
                begin_run(&state).unwrap();
                let _active_run = ActiveRunGuard(state.clone());
                panic!("worker panic");
            }
        }));

        assert!(result.is_err());
        assert!(state.active.lock().unwrap().is_none());
        assert!(begin_run(&state).is_ok());
    }
}
