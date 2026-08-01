//! Backend-owned AutoScan lifecycle.
//!
//! The webview is a subscriber, never the clock. One generation owns one exact job identity,
//! revision and target. Local macOS roots use FSEvents as a trigger plus a periodic full verification; remote
//! roots and unsupported platforms say explicitly that they are using interval polling.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::Emitter;

use crate::dto::CompareOwner;

const DEFAULT_INTERVAL_SECS: u64 = 30;
const WORKER_TICK: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AutoScanBinding {
    pub(crate) job_id: String,
    pub(crate) job_name: String,
    pub(crate) config_revision: String,
    pub(crate) target_index: usize,
    pub(crate) interval_secs: u64,
    pub(crate) auto_apply: bool,
    pub(crate) rigor: String,
}

impl AutoScanBinding {
    pub(crate) fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs.max(1))
    }

    fn checkpoint_owner(&self) -> String {
        format!(
            "autoscan-v2\0{}\0{}\0{}",
            self.job_id, self.config_revision, self.target_index
        )
    }

    pub(crate) fn owns_compare(&self, owner: &CompareOwner) -> bool {
        owner.job_id == self.job_id
            && owner.config_revision == self.config_revision
            && owner.target_index == self.target_index
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AutoScanMode {
    Starting,
    NativeFsevents,
    Polling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AutoScanReason {
    Bootstrap,
    FilesystemChange,
    WatchInvalidated,
    PeriodicVerification,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AutoScanStatusDto {
    pub(crate) active: bool,
    pub(crate) generation: u64,
    pub(crate) job_id: Option<String>,
    pub(crate) job_name: Option<String>,
    pub(crate) config_revision: Option<String>,
    pub(crate) target_index: Option<usize>,
    pub(crate) interval_secs: Option<u64>,
    pub(crate) auto_apply: bool,
    pub(crate) mode: Option<AutoScanMode>,
    pub(crate) detail: String,
    pub(crate) active_ticket: Option<u64>,
}

impl AutoScanStatusDto {
    fn inactive() -> Self {
        Self {
            active: false,
            generation: 0,
            job_id: None,
            job_name: None,
            config_revision: None,
            target_index: None,
            interval_secs: None,
            auto_apply: false,
            mode: None,
            detail: "AutoScan is off".into(),
            active_ticket: None,
        }
    }

    fn starting(generation: u64, binding: &AutoScanBinding) -> Self {
        Self {
            active: true,
            generation,
            job_id: Some(binding.job_id.clone()),
            job_name: Some(binding.job_name.clone()),
            config_revision: Some(binding.config_revision.clone()),
            target_index: Some(binding.target_index),
            interval_secs: Some(binding.interval_secs),
            auto_apply: binding.auto_apply,
            mode: Some(AutoScanMode::Starting),
            detail: "Preparing backend-owned change detection".into(),
            active_ticket: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AutoScanTriggerDto {
    pub(crate) generation: u64,
    pub(crate) ticket_id: u64,
    pub(crate) job_id: String,
    pub(crate) job_name: String,
    pub(crate) config_revision: String,
    pub(crate) target_index: usize,
    pub(crate) auto_apply: bool,
    pub(crate) mode: AutoScanMode,
    pub(crate) reason: AutoScanReason,
}

enum WorkerCommand {
    Complete { ticket_id: u64, succeeded: bool },
    Stop,
}

struct ActiveAutoScan {
    binding: AutoScanBinding,
    generation: u64,
    commands: mpsc::Sender<WorkerCommand>,
    status: Arc<Mutex<AutoScanStatusDto>>,
    join: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub(crate) struct AutoScanController {
    gate: Mutex<()>,
    active: Mutex<Option<ActiveAutoScan>>,
    generations: AtomicU64,
}

impl AutoScanController {
    pub(crate) fn start(
        &self,
        app: tauri::AppHandle,
        binding: AutoScanBinding,
        local_roots: Option<(PathBuf, PathBuf)>,
    ) -> AutoScanStatusDto {
        let _gate = self.gate.lock().unwrap();
        self.stop_locked();

        let generation = self.generations.fetch_add(1, Ordering::Relaxed) + 1;
        let initial = AutoScanStatusDto::starting(generation, &binding);
        let status = Arc::new(Mutex::new(initial.clone()));
        let (commands, receiver) = mpsc::channel();
        let worker_status = status.clone();
        let worker_binding = binding.clone();
        let join = std::thread::Builder::new()
            .name(format!("syncdash-autoscan-{generation}"))
            .spawn(move || {
                run_worker(
                    &app,
                    generation,
                    worker_binding,
                    local_roots,
                    receiver,
                    worker_status,
                );
            })
            .expect("AutoScan worker thread creation failed");
        *self.active.lock().unwrap() = Some(ActiveAutoScan {
            binding,
            generation,
            commands,
            status,
            join: Some(join),
        });
        initial
    }

    pub(crate) fn stop(&self) -> AutoScanStatusDto {
        let _gate = self.gate.lock().unwrap();
        self.stop_locked();
        AutoScanStatusDto::inactive()
    }

    fn stop_locked(&self) {
        let Some(mut active) = self.active.lock().unwrap().take() else {
            return;
        };
        let _ = active.commands.send(WorkerCommand::Stop);
        if let Some(join) = active.join.take() {
            let _ = join.join();
        }
    }

    pub(crate) fn stop_if_job_id(&self, job_id: &str) -> bool {
        let _gate = self.gate.lock().unwrap();
        let matches = self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|active| active.binding.job_id == job_id);
        if matches {
            self.stop_locked();
        }
        matches
    }

    /// Rename the display label of an active identity without disturbing its watcher generation or
    /// outstanding ticket. Worker ownership and checkpoints are identity-based, so no scan evidence
    /// changes when the registry filename changes.
    pub(crate) fn rebind_job_name(
        &self,
        job_id: &str,
        job_name: &str,
    ) -> Option<AutoScanStatusDto> {
        let _gate = self.gate.lock().unwrap();
        let mut active = self.active.lock().unwrap();
        let active = active
            .as_mut()
            .filter(|active| active.binding.job_id == job_id)?;
        active.binding.job_name = job_name.to_string();
        let mut status = active.status.lock().ok()?;
        status.job_name = Some(job_name.to_string());
        Some(status.clone())
    }

    pub(crate) fn status(&self) -> AutoScanStatusDto {
        self.active
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|active| active.status.lock().ok().map(|status| status.clone()))
            .unwrap_or_else(AutoScanStatusDto::inactive)
    }

    pub(crate) fn complete(
        &self,
        generation: u64,
        ticket_id: u64,
        succeeded: bool,
        owner: Option<&CompareOwner>,
    ) -> Result<AutoScanStatusDto, String> {
        let active_guard = self.active.lock().unwrap();
        let active = active_guard
            .as_ref()
            .ok_or_else(|| "AutoScan is no longer active".to_string())?;
        if active.generation != generation {
            return Err("This AutoScan generation is no longer active".into());
        }
        if succeeded {
            let owner = owner.ok_or_else(|| {
                "A successful AutoScan compare must provide its authenticated result owner"
                    .to_string()
            })?;
            if !active.binding.owns_compare(owner) {
                return Err(
                    "The compare result does not belong to this AutoScan job identity, revision, and target"
                        .into(),
                );
            }
        }
        let mut status = active
            .status
            .lock()
            .map_err(|_| "AutoScan status lock is poisoned".to_string())?;
        if status.active_ticket != Some(ticket_id) {
            return Err("This AutoScan work ticket is no longer awaiting completion".into());
        }
        // Consume the completion right here. Concurrent duplicate calls cannot both reach the
        // worker even if the webview retries after losing an IPC response.
        status.active_ticket = None;
        active
            .commands
            .send(WorkerCommand::Complete {
                ticket_id,
                succeeded,
            })
            .map_err(|_| "The AutoScan worker has stopped".to_string())?;
        Ok(status.clone())
    }
}

impl Drop for AutoScanController {
    fn drop(&mut self) {
        if let Ok(active) = self.active.get_mut() {
            if let Some(active) = active.as_mut() {
                let _ = active.commands.send(WorkerCommand::Stop);
            }
        }
    }
}

fn publish_status(
    app: &tauri::AppHandle,
    status: &Arc<Mutex<AutoScanStatusDto>>,
    mode: AutoScanMode,
    detail: impl Into<String>,
) {
    let snapshot = {
        let mut status = status.lock().unwrap();
        status.mode = Some(mode);
        status.detail = detail.into();
        status.clone()
    };
    let _ = app.emit("autoscan-status", snapshot);
}

fn publish_trigger(
    app: &tauri::AppHandle,
    status: &Arc<Mutex<AutoScanStatusDto>>,
    generation: u64,
    binding: &AutoScanBinding,
    ticket_id: u64,
    mode: AutoScanMode,
    reason: AutoScanReason,
) -> bool {
    let job_name = match resolve_binding_job_name(binding) {
        Ok(job_name) => job_name,
        Err(error) => {
            let snapshot = {
                let mut status = status.lock().unwrap();
                status.active = false;
                status.active_ticket = None;
                status.detail = format!("AutoScan stopped safely: {error}");
                status.clone()
            };
            let _ = app.emit("autoscan-status", snapshot);
            return false;
        }
    };
    {
        let mut status = status.lock().unwrap();
        status.active_ticket = Some(ticket_id);
        status.job_name = Some(job_name.clone());
        status.mode = Some(mode);
        status.detail = match reason {
            AutoScanReason::Bootstrap => "Running the initial verification".into(),
            AutoScanReason::FilesystemChange => "A filesystem change requested verification".into(),
            AutoScanReason::WatchInvalidated => {
                "The native event history changed; a full verification is required".into()
            }
            AutoScanReason::PeriodicVerification => "Running the periodic full verification".into(),
        };
    }
    let _ = app.emit(
        "autoscan-trigger",
        AutoScanTriggerDto {
            generation,
            ticket_id,
            job_id: binding.job_id.clone(),
            job_name,
            config_revision: binding.config_revision.clone(),
            target_index: binding.target_index,
            auto_apply: binding.auto_apply,
            mode,
            reason,
        },
    );
    true
}

fn resolve_binding_job_name(binding: &AutoScanBinding) -> Result<String, String> {
    let (job_name, job) = syncdash::job::load_by_id(&binding.job_id).map_err(|error| {
        format!(
            "job '{}' no longer has registry identity '{}': {error}",
            binding.job_name, binding.job_id
        )
    })?;
    validate_resolved_binding(binding, &job_name, &job)?;
    Ok(job_name)
}

fn validate_resolved_binding(
    binding: &AutoScanBinding,
    job_name: &str,
    job: &syncdash::job::Job,
) -> Result<(), String> {
    if job.job_id != binding.job_id {
        return Err(format!(
            "job name '{job_name}' now belongs to a replacement identity"
        ));
    }
    let revision = syncdash::job::config_revision(job)
        .map_err(|error| format!("job '{job_name}' cannot be fingerprinted: {error}"))?;
    if revision != binding.config_revision {
        return Err(format!(
            "job '{job_name}' changed configuration after this AutoScan generation started"
        ));
    }
    if binding.target_index >= job.target_list().len() {
        return Err(format!(
            "job '{job_name}' no longer has target {}",
            binding.target_index + 1
        ));
    }
    Ok(())
}

fn run_worker(
    app: &tauri::AppHandle,
    generation: u64,
    binding: AutoScanBinding,
    local_roots: Option<(PathBuf, PathBuf)>,
    commands: mpsc::Receiver<WorkerCommand>,
    status: Arc<Mutex<AutoScanStatusDto>>,
) {
    #[cfg(target_os = "macos")]
    if let Some((source, target)) = local_roots {
        match run_native_macos(
            app, generation, &binding, &source, &target, &commands, &status,
        ) {
            NativeExit::Stopped => return,
            NativeExit::Fallback {
                detail,
                next_ticket,
            } => {
                run_polling(
                    app,
                    generation,
                    &binding,
                    &commands,
                    &status,
                    detail,
                    next_ticket,
                    false,
                );
                return;
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    let fallback = if local_roots.is_some() {
        "Native filesystem events are not available on this platform; polling while SyncDash is open"
    } else {
        "Remote roots do not expose a local event journal; polling while SyncDash is open"
    };
    #[cfg(target_os = "macos")]
    let fallback =
        "Remote roots do not expose a local FSEvents journal; polling while SyncDash is open";
    run_polling(
        app,
        generation,
        &binding,
        &commands,
        &status,
        fallback.into(),
        1,
        true,
    );
}

fn run_polling(
    app: &tauri::AppHandle,
    generation: u64,
    binding: &AutoScanBinding,
    commands: &mpsc::Receiver<WorkerCommand>,
    status: &Arc<Mutex<AutoScanStatusDto>>,
    detail: String,
    mut next_ticket: u64,
    immediate: bool,
) {
    publish_status(app, status, AutoScanMode::Polling, detail);
    let interval = binding.interval();
    let mut deadline = if immediate {
        Instant::now()
    } else {
        Instant::now() + interval
    };
    let mut awaiting = status.lock().unwrap().active_ticket;
    loop {
        match commands.recv_timeout(WORKER_TICK) {
            Ok(WorkerCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(WorkerCommand::Complete {
                ticket_id,
                succeeded: _,
            }) => {
                if awaiting == Some(ticket_id) {
                    awaiting = None;
                    deadline = Instant::now() + interval;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if awaiting.is_none() && Instant::now() >= deadline {
            let reason = if next_ticket == 1 {
                AutoScanReason::Bootstrap
            } else {
                AutoScanReason::PeriodicVerification
            };
            if !publish_trigger(
                app,
                status,
                generation,
                binding,
                next_ticket,
                AutoScanMode::Polling,
                reason,
            ) {
                return;
            }
            awaiting = Some(next_ticket);
            next_ticket = next_ticket.wrapping_add(1).max(1);
        }
    }
}

#[cfg(target_os = "macos")]
enum NativeExit {
    Stopped,
    Fallback { detail: String, next_ticket: u64 },
}

#[cfg(target_os = "macos")]
fn run_native_macos(
    app: &tauri::AppHandle,
    generation: u64,
    binding: &AutoScanBinding,
    source: &std::path::Path,
    target: &std::path::Path,
    commands: &mpsc::Receiver<WorkerCommand>,
    status: &Arc<Mutex<AutoScanStatusDto>>,
) -> NativeExit {
    use syncdash::fs::watch::WatchMessage;
    use syncdash::run::watch::{
        ChangeBatch, FullScanReason, RigorPolicy, WatchConfig, WatchTrigger, WorkCoverage,
    };
    use syncdash::store::watch::{CheckpointLoad, CheckpointStore};

    let checkpoint = CheckpointStore::for_job(binding.checkpoint_owner());
    let resume = match checkpoint.load() {
        Ok(CheckpointLoad::Valid(position)) => Some(position),
        Ok(CheckpointLoad::Missing) => None,
        Ok(CheckpointLoad::Invalid(reason)) => {
            syncdash::log_warn!("autoscan", "Ignoring invalid watch checkpoint: {reason}");
            None
        }
        Err(error) => {
            syncdash::log_warn!("autoscan", "Cannot read watch checkpoint: {error}");
            None
        }
    };
    let watcher = match syncdash::fs::watch::macos::watch_pair(source, target, resume.as_ref()) {
        Ok(watcher) => watcher,
        Err(error) => {
            return NativeExit::Fallback {
                detail: format!(
                "FSEvents could not arm both local roots ({error}); polling while SyncDash is open"
            ),
                next_ticket: 1,
            }
        }
    };
    let policy = match binding.rigor.as_str() {
        "quick" => RigorPolicy::Quick,
        "fast" => RigorPolicy::Fast,
        "standard" => RigorPolicy::Standard,
        "paranoid" => RigorPolicy::Paranoid,
        _ => RigorPolicy::Balanced,
    };
    let mut trigger = WatchTrigger::new(policy, WatchConfig::default());
    if let Some(position) = resume {
        if let Err(error) = trigger.restore_checkpoint(position) {
            syncdash::log_warn!("autoscan", "Cannot restore watch checkpoint: {error}");
        }
    }
    if let Err(error) = trigger.arm(watcher.armed().position.clone()) {
        return NativeExit::Fallback {
            detail: format!(
                "FSEvents returned an invalid cursor ({error}); polling while SyncDash is open"
            ),
            next_ticket: 1,
        };
    }
    publish_status(
        app,
        status,
        AutoScanMode::NativeFsevents,
        "Watching both local roots with FSEvents; periodic full verification remains enabled",
    );

    let interval = binding.interval();
    let mut periodic_deadline = Instant::now() + interval;
    let started = Instant::now();
    let mut retry_not_before = started;
    let mut next_ticket = 1u64;
    loop {
        loop {
            match commands.try_recv() {
                Ok(WorkerCommand::Stop) | Err(mpsc::TryRecvError::Disconnected) => {
                    return NativeExit::Stopped
                }
                Ok(WorkerCommand::Complete {
                    ticket_id,
                    succeeded,
                }) => {
                    let completed = if succeeded {
                        trigger.complete_success(ticket_id, |position| checkpoint.save(position))
                    } else {
                        trigger
                            .complete_failure(ticket_id)
                            .map_err(std::io::Error::other)
                    };
                    if let Err(error) = completed {
                        syncdash::log_warn!("autoscan", "AutoScan work was not committed: {error}");
                    }
                    periodic_deadline = Instant::now() + interval;
                    retry_not_before = periodic_deadline;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }

        match watcher.receiver().recv_timeout(WORKER_TICK) {
            Ok(WatchMessage::Trigger(batch)) => {
                let at_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                if let Err(error) = trigger.observe(ChangeBatch::from(batch), at_ms) {
                    return NativeExit::Fallback {
                        detail: format!(
                            "FSEvents cursor continuity failed ({error}); polling while SyncDash is open"
                        ),
                        next_ticket,
                    };
                }
            }
            Ok(WatchMessage::BackendError { message, .. }) => {
                return NativeExit::Fallback {
                    detail: format!(
                        "FSEvents stopped reporting reliably ({message}); polling while SyncDash is open"
                    ),
                    next_ticket,
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return NativeExit::Fallback {
                    detail: "FSEvents disconnected; polling while SyncDash is open".into(),
                    next_ticket,
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        if Instant::now() >= periodic_deadline {
            match watcher.current_position() {
                Ok(position) => {
                    let at_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                    if let Err(error) = trigger.request_periodic(position, at_ms) {
                        return NativeExit::Fallback {
                            detail: format!(
                                "FSEvents periodic cursor capture failed ({error}); polling while SyncDash is open"
                            ),
                            next_ticket,
                        };
                    }
                }
                Err(error) => {
                    return NativeExit::Fallback {
                        detail: format!(
                        "FSEvents cursor capture failed ({error}); polling while SyncDash is open"
                    ),
                        next_ticket,
                    }
                }
            }
            periodic_deadline = Instant::now() + interval;
        }

        if Instant::now() < retry_not_before {
            continue;
        }
        let now_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let ticket = match trigger.next_work(now_ms) {
            Ok(Some(ticket)) => ticket,
            Ok(None) => continue,
            Err(error) => {
                return NativeExit::Fallback {
                    detail: format!(
                        "FSEvents orchestration failed ({error}); polling while SyncDash is open"
                    ),
                    next_ticket,
                }
            }
        };
        let reason = match ticket.coverage {
            WorkCoverage::FullTree {
                reason: FullScanReason::Bootstrap,
            } => AutoScanReason::Bootstrap,
            WorkCoverage::FullTree {
                reason: FullScanReason::Periodic,
            } => AutoScanReason::PeriodicVerification,
            WorkCoverage::FullTree {
                reason: FullScanReason::WatchInvalidated(_),
            }
            | WorkCoverage::FullTree {
                reason: FullScanReason::ChangeSetTooLarge { .. },
            } => AutoScanReason::WatchInvalidated,
            WorkCoverage::FullTree { .. } | WorkCoverage::IncrementalEligible { .. } => {
                AutoScanReason::FilesystemChange
            }
        };
        next_ticket = next_ticket.max(ticket.id.wrapping_add(1).max(1));
        if !publish_trigger(
            app,
            status,
            generation,
            binding,
            ticket.id,
            AutoScanMode::NativeFsevents,
            reason,
        ) {
            return NativeExit::Stopped;
        }
    }
}

pub(crate) fn configured_interval(job: &syncdash::job::Job) -> u64 {
    job.watch_interval_secs
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> AutoScanBinding {
        AutoScanBinding {
            job_id: "job-id-photos".into(),
            job_name: "photos".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
            interval_secs: 30,
            auto_apply: false,
            rigor: "standard".into(),
        }
    }

    #[test]
    fn compare_ownership_includes_revision_and_target() {
        let binding = binding();
        let owner = CompareOwner {
            compare_id: 7,
            job_id: "job-id-photos".into(),
            job_name: "photos".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
        };
        assert!(binding.owns_compare(&owner));
        assert!(binding.owns_compare(&CompareOwner {
            job_name: "renamed".into(),
            ..owner.clone()
        }));
        assert!(!binding.owns_compare(&CompareOwner {
            job_id: "replacement-id".into(),
            ..owner.clone()
        }));
        assert!(!binding.owns_compare(&CompareOwner {
            config_revision: "revision-b".into(),
            ..owner.clone()
        }));
        assert!(!binding.owns_compare(&CompareOwner {
            target_index: 0,
            ..owner
        }));
    }

    #[test]
    fn configured_interval_is_explicit_and_never_zero() {
        let mut job = syncdash::job::Job::default();
        assert_eq!(configured_interval(&job), DEFAULT_INTERVAL_SECS);
        job.watch_interval_secs = Some(0);
        assert_eq!(configured_interval(&job), 1);
        job.watch_interval_secs = Some(90);
        assert_eq!(configured_interval(&job), 90);
    }

    fn controller_waiting_for(
        ticket_id: u64,
    ) -> (AutoScanController, mpsc::Receiver<WorkerCommand>) {
        let controller = AutoScanController::default();
        let binding = binding();
        let (commands, receiver) = mpsc::channel();
        let mut status = AutoScanStatusDto::starting(4, &binding);
        status.active_ticket = Some(ticket_id);
        *controller.active.lock().unwrap() = Some(ActiveAutoScan {
            binding,
            generation: 4,
            commands,
            status: Arc::new(Mutex::new(status)),
            join: None,
        });
        (controller, receiver)
    }

    fn owner() -> CompareOwner {
        CompareOwner {
            compare_id: 8,
            job_id: "job-id-photos".into(),
            job_name: "photos".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
        }
    }

    #[test]
    fn completion_is_one_use_and_requires_the_exact_generation_and_ticket() {
        let (controller, receiver) = controller_waiting_for(11);
        assert!(controller.complete(3, 11, false, None).is_err());
        assert!(controller.complete(4, 12, false, None).is_err());
        controller.complete(4, 11, false, None).unwrap();
        assert!(controller.complete(4, 11, false, None).is_err());
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::Complete {
                ticket_id: 11,
                succeeded: false
            })
        ));
    }

    #[test]
    fn successful_completion_requires_an_authenticated_owned_compare() {
        let (controller, receiver) = controller_waiting_for(12);
        let mut wrong = owner();
        wrong.target_index = 0;
        assert!(controller.complete(4, 12, true, None).is_err());
        assert!(controller.complete(4, 12, true, Some(&wrong)).is_err());
        assert!(receiver.try_recv().is_err());
        controller.complete(4, 12, true, Some(&owner())).unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::Complete {
                ticket_id: 12,
                succeeded: true
            })
        ));
    }

    #[test]
    fn pure_rename_rebinds_status_without_consuming_generation_or_ticket() {
        let (controller, _receiver) = controller_waiting_for(13);
        assert!(controller
            .rebind_job_name("replacement-id", "ignored")
            .is_none());

        let status = controller
            .rebind_job_name("job-id-photos", "renamed")
            .expect("the active identity should be rebound");
        assert_eq!(status.generation, 4);
        assert_eq!(status.active_ticket, Some(13));
        assert_eq!(status.job_id.as_deref(), Some("job-id-photos"));
        assert_eq!(status.job_name.as_deref(), Some("renamed"));
        assert_eq!(
            controller
                .active
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .binding
                .job_name,
            "renamed"
        );
    }

    #[test]
    fn resolved_binding_accepts_rename_but_rejects_same_name_replacement() {
        let mut registered = syncdash::job::Job::default();
        registered.job_id = "job-id-original".into();
        let revision = syncdash::job::config_revision(&registered).unwrap();
        let mut binding = binding();
        binding.job_id = registered.job_id.clone();
        binding.job_name = "old-name".into();
        binding.config_revision = revision.clone();
        binding.target_index = 0;

        validate_resolved_binding(&binding, "renamed", &registered).unwrap();

        registered.job_id = "job-id-replacement".into();
        let error = validate_resolved_binding(&binding, "old-name", &registered).unwrap_err();
        assert!(error.contains("replacement identity"), "{error}");
    }
}
