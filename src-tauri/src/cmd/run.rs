//! Running a job: compare, preflight, apply, and the cancel/pause controls over a live run.
//!
//! The transport choice — this process or an ssh peer — is `run`'s, not this module's. These call
//! `run::compare` / `run::preflight` / `run::apply` and let the router decide.

use std::sync::Arc;

use syncdash::model::plan::{Action, Plan};
use syncdash::pipeline::compare;
use syncdash::{job, run};
use tauri::Emitter;

use crate::bridge::{make_ctx, RunEvent, RunEventRepository};
use crate::dto::{ApplyDto, CompareOwner, PlanDto, PreflightDto, SelectedRowDto};
use crate::state::{
    begin_run, begin_run_command, begin_run_for_launch, end_run, finish_run_command,
    release_progress_launch, request_cancel, resolve_selected_ops, resolve_target, set_paused,
    user_err, validate_cached_compare, CachedCompare, CompareProvenance, ResultKey,
    ResultRepository, RunState,
};

#[derive(Clone, serde::Serialize)]
struct RunRejected {
    launch_id: u64,
    message: String,
}

struct ActiveRunGuard {
    state: Arc<RunState>,
    run_id: u64,
}

struct RunCommandGuard(Arc<RunState>);

struct AppliedResultGuard {
    results: Arc<ResultRepository>,
    job_name: String,
    config_revision: String,
    invalidate_on_drop: bool,
}

impl AppliedResultGuard {
    fn new(results: Arc<ResultRepository>, job_name: &str, config_revision: &str) -> Self {
        Self {
            results,
            job_name: job_name.to_string(),
            config_revision: config_revision.to_string(),
            invalidate_on_drop: true,
        }
    }

    fn retain_for_safe_rejection(&mut self) {
        self.invalidate_on_drop = false;
    }
}

impl Drop for AppliedResultGuard {
    fn drop(&mut self) {
        if self.invalidate_on_drop {
            self.results
                .0
                .lock()
                .unwrap()
                .invalidate_revision(&self.job_name, &self.config_revision);
        }
    }
}

impl RunCommandGuard {
    fn begin(state: Arc<RunState>) -> Self {
        begin_run_command(&state);
        Self(state)
    }
}

impl Drop for RunCommandGuard {
    fn drop(&mut self) {
        finish_run_command(&self.0);
    }
}

impl Drop for ActiveRunGuard {
    fn drop(&mut self) {
        end_run(&self.state, self.run_id);
    }
}

/// Request cooperative cancellation of the active run. Returns whether an active run existed.
#[tauri::command]
pub fn cancel_run(state: tauri::State<'_, Arc<RunState>>, run_id: u64) -> Result<bool, String> {
    request_cancel(state.inner(), run_id)
}

/// Pause/resume the active run (elapsed stops growing while paused, the RootLock heartbeat keeps beating)
#[tauri::command]
pub fn pause_run(
    state: tauri::State<'_, Arc<RunState>>,
    run_id: u64,
    paused: bool,
) -> Result<bool, String> {
    set_paused(state.inner(), run_id, paused)
}

#[tauri::command]
pub fn replay_run_events(
    events: tauri::State<'_, Arc<RunEventRepository>>,
    purpose: String,
    after_sequence: Option<u64>,
) -> Result<Vec<RunEvent>, String> {
    if !matches!(purpose.as_str(), "compare" | "apply") {
        return Err(format!("Unknown run purpose: {purpose}"));
    }
    Ok(events.replay(&purpose, after_sequence.unwrap_or(0)))
}

#[tauri::command]
pub async fn compare_job(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<RunState>>,
    results: tauri::State<'_, Arc<ResultRepository>>,
    events: tauri::State<'_, Arc<RunEventRepository>>,
    name: String,
    target_index: Option<usize>,
    accept_caps: Option<bool>,
) -> Result<PlanDto, String> {
    let st = state.inner().clone();
    let _command = RunCommandGuard::begin(st.clone());
    let results = results.inner().clone();
    let events = events.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (job_name, full_job) = job::load_named(&name).map_err(|e| e.to_string())?;
        let config_revision =
            job::config_revision(&full_job).map_err(|e| format!("Job '{job_name}': {e}"))?;
        let (target_index, job) = resolve_target(&full_job, target_index)?;
        let (run_id, ctl) = begin_run(&st)?;
        let _active_run = ActiveRunGuard { state: st.clone(), run_id };
        let ctx = make_ctx(&app, events, run_id, ctl, "compare");
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
        let r = run::compare(&job_name, &job, &ctx, accept_caps.unwrap_or(false));
        // compare has no side effects: one index line, no directory. A 30s watch cycle = 2880 runs a day,
        // and creating a directory each time would flood the log disk.
        syncdash::obs::runlog::compare_summary(
            &job_name,
            &run::run_kind(&job, "compare"),
            ts_ms,
            r.as_ref().map(|o| o.plan.ops.len() as u64).unwrap_or(0),
            t0.elapsed().as_millis() as u64,
            r.as_ref().err().map(syncdash::obs::progress::is_cancelled).unwrap_or(false),
        );
        let out = r.map_err(user_err)?;
        let plan_digest = out.plan.digest();
        let owner = CompareOwner {
            compare_id: run_id,
            job_name,
            target_index,
            config_revision,
        };
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
            owner: owner.clone(),
            header: out.plan.header,
            ops: out.plan.ops,
            metas,
            equal_count: ev.equal_count,
            equal_bytes: ev.equal_bytes,
        };
        // Publish only after compare and evidence construction both succeeded. Hold the repository
        // while re-reading the registered revision: an in-app save/delete publishes its invalidation
        // under this same lock, so an old result cannot be inserted behind a completed mutation.
        let mut repository = results.0.lock().unwrap();
        let (_, current_job) = job::load_named(&owner.job_name).map_err(|error| {
            format!("Job '{}' changed while Compare was running: {error}", owner.job_name)
        })?;
        let current_revision = job::config_revision(&current_job)
            .map_err(|error| format!("Job '{}': {error}", owner.job_name))?;
        if current_revision != owner.config_revision {
            return Err(format!(
                "Job '{}' changed while Compare was running — run Compare again",
                owner.job_name
            ));
        }
        repository.insert(CachedCompare {
            provenance: CompareProvenance { owner, plan_digest },
            plan: dto.clone(),
            source: out.source,
            target: out.target,
        });
        Ok(dto)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Pre-sync gate checks (disk space / deletion ratio). The frontend shows the result in the confirmation sheet,
/// so "why won't it let me sync" has somewhere to be answered instead of only appearing on stderr.
#[tauri::command]
pub async fn preflight(
    results: tauri::State<'_, Arc<ResultRepository>>,
    name: String,
    plan: PlanDto,
    selected: Vec<SelectedRowDto>,
    acknowledged: bool,
    target_index: Option<usize>,
) -> Result<PreflightDto, String> {
    let results = results.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if name != plan.owner.job_name {
            return Err(format!(
                "This compare result belongs to job '{}', not '{}' — run Compare again",
                plan.owner.job_name, name
            ));
        }
        let (job_name, full_job) = job::load_named(&name).map_err(|e| e.to_string())?;
        let config_revision =
            job::config_revision(&full_job).map_err(|e| format!("Job '{job_name}': {e}"))?;
        let (target_index, job) = resolve_target(&full_job, target_index)?;
        let full = Plan { header: plan.header.clone(), ops: plan.ops.clone() };
        let plan_digest = full.digest();
        {
            let mut repository = results.0.lock().unwrap();
            let key = ResultKey::new(&job_name, target_index, &config_revision);
            let cached = repository.get(&key);
            validate_cached_compare(
                cached.map(|result| &result.provenance),
                &plan.owner,
                &job_name,
                target_index,
                &config_revision,
                Some(&plan_digest),
            )?;
        }
        let ops = resolve_selected_ops(&full.ops, &selected)?;
        // A peer job only gets the deletion-ratio gate: disk space and the marker live on the
        // remote machine, so checking them here would be answering about the wrong disk.
        let unacknowledged = run::preflight(&job, &full, &ops, false).map_err(user_err)?;
        let acknowledged_verdict = if unacknowledged.ok() {
            None
        } else {
            Some(run::preflight(&job, &full, &ops, true).map_err(user_err)?)
        };
        let acknowledgeable = acknowledged_verdict
            .as_ref()
            .is_some_and(syncdash::pipeline::guard::Verdict::ok);
        let verdict = if acknowledged {
            acknowledged_verdict.unwrap_or(unacknowledged)
        } else {
            unacknowledged
        };
        Ok(PreflightDto {
            ok: verdict.ok(),
            acknowledgeable,
            blockers: verdict.blockers,
            warnings: verdict.warnings,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn apply_job(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<RunState>>,
    results: tauri::State<'_, Arc<ResultRepository>>,
    events: tauri::State<'_, Arc<RunEventRepository>>,
    name: String,
    plan: PlanDto,
    selected: Vec<SelectedRowDto>,
    acknowledged: bool,
    target_index: Option<usize>,
    accept_caps: Option<bool>,
    launch_id: Option<u64>,
) -> Result<ApplyDto, String> {
    let st = state.inner().clone();
    let _command = RunCommandGuard::begin(st.clone());
    let results = results.inner().clone();
    let events = events.inner().clone();
    let reject_state = state.inner().clone();
    let requested_launch = launch_id;
    let run_app = app.clone();
    let joined = tauri::async_runtime::spawn_blocking(move || -> Result<ApplyDto, (String, bool)> {
        if name != plan.owner.job_name {
            return Err((
                format!(
                    "This compare result belongs to job '{}', not '{}' — run Compare again",
                    plan.owner.job_name, name
                ),
                false,
            ));
        }
        let (job_name, full_job) =
            job::load_named(&name).map_err(|e| (e.to_string(), false))?;
        let config_revision = job::config_revision(&full_job)
            .map_err(|e| (format!("Job '{job_name}': {e}"), false))?;
        let (target_index, job) = resolve_target(&full_job, target_index).map_err(|e| (e, false))?;
        let full = Plan { header: plan.header.clone(), ops: plan.ops.clone() };
        let plan_digest = full.digest();
        // Keep the repository stable through the gate and run reservation. If a compare is already
        // scanning, begin_run rejects this apply before a write; if this apply reserves first, no
        // compare can replace the authenticated job/target/revision entry before execution starts.
        let mut repository = results.0.lock().unwrap();
        let key = ResultKey::new(&job_name, target_index, &config_revision);
        let cached = repository.get(&key);
        validate_cached_compare(
            cached.map(|result| &result.provenance),
            &plan.owner,
            &job_name,
            target_index,
            &config_revision,
            Some(&plan_digest),
        )
        .map_err(|e| (e, false))?;
        let ops = resolve_selected_ops(&full.ops, &selected).map_err(|e| (e, false))?;
        // Touch nothing when a gate fails, and hand the reason back to the UI
        let v = run::preflight(&job, &full, &ops, acknowledged)
            .map_err(|error| (user_err(error), false))?;
        if !v.ok() {
            return Err((v.blockers.join("\n"), false));
        }
        let (run_id, ctl) = begin_run_for_launch(&st, launch_id).map_err(|e| (e, false))?;
        drop(repository);
        let mut applied_result =
            AppliedResultGuard::new(results.clone(), &job_name, &config_revision);
        let _active_run = ActiveRunGuard { state: st.clone(), run_id };
        let ctx = make_ctx(&run_app, events, run_id, ctl, "apply");
        // M4: every real apply writes a run-log entry (the Recorder also collects error events into the detail file)
        let t0 = std::time::Instant::now();
        let rec = syncdash::obs::runlog::Recorder::start(&job_name, &run::run_kind(&job, "apply"), &ctx, &ops);
        // A degraded apply without consent refuses with the NeedsAck lines; the frontend shows
        // them and re-invokes with accept_caps=true if the user agrees.
        let out = match run::apply(
            &job_name, &job, &full, &ops, None, false, acknowledged, accept_caps.unwrap_or(false), &rec.ctx,
        ) {
            Ok(o) => o,
            Err(e) => {
                let message = user_err(e);
                if message.contains("--accept-caps") {
                    applied_result.retain_for_safe_rejection();
                }
                return Err((message, true));
            }
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
                let (run_id, _) = begin_run(&state).unwrap();
                let _active_run = ActiveRunGuard { state: state.clone(), run_id };
                panic!("worker panic");
            }
        }));

        assert!(result.is_err());
        assert!(begin_run(&state).is_ok());
    }
}
