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
use crate::state::RunState;

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
        owner.identity.job_id == self.job_id
            && owner.identity.config_revision == self.config_revision
            && owner.identity.target_index == self.target_index
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
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
    /// Monotonic within one generation and retained after completion, so delayed same-generation
    /// IPC snapshots cannot make older ticket state appear current.
    pub(crate) latest_ticket_id: u64,
    pub(crate) active_ticket: Option<u64>,
    pub(crate) pending_trigger: Option<AutoScanTriggerDto>,
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
            latest_ticket_id: 0,
            active_ticket: None,
            pending_trigger: None,
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
            latest_ticket_id: 0,
            active_ticket: None,
            pending_trigger: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
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

/// One successful AutoScan Compare that may be promoted into exactly one AutoApply authorization.
/// Display names are relabelled on rename; every authority-bearing field is stable identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutoApplyTicket {
    pub(crate) generation: u64,
    pub(crate) ticket_id: u64,
    pub(crate) job_id: String,
    pub(crate) job_name: String,
    pub(crate) config_revision: String,
    pub(crate) target_index: usize,
    pub(crate) owner: CompareOwner,
}

impl AutoApplyTicket {
    fn matches_key(&self, generation: u64, ticket_id: u64) -> bool {
        self.generation == generation && self.ticket_id == ticket_id
    }

    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.ticket_id == other.ticket_id
            && self.job_id == other.job_id
            && self.config_revision == other.config_revision
            && self.target_index == other.target_index
            && self.owner.identity == other.owner.identity
    }

    fn rebind_name(&mut self, job_name: &str) {
        self.job_name = job_name.to_string();
        self.owner.job_name = job_name.to_string();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoApplyTicketStage {
    Completed,
    Claimed,
    Authorized,
}

#[derive(Clone, Debug)]
struct AutoApplyTicketRecord {
    stage: AutoApplyTicketStage,
    ticket: AutoApplyTicket,
}

struct AutoScanShared {
    status: AutoScanStatusDto,
    pending_compare_min_run_id: Option<u64>,
    pending_compare_owner: Option<CompareOwner>,
    auto_apply_ticket: Option<AutoApplyTicketRecord>,
}

enum WorkerCommand {
    Complete { ticket_id: u64, succeeded: bool },
    Stop,
}

struct ActiveAutoScan {
    binding: AutoScanBinding,
    generation: u64,
    commands: mpsc::Sender<WorkerCommand>,
    shared: Arc<Mutex<AutoScanShared>>,
    join: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub(crate) struct AutoScanController {
    gate: Mutex<()>,
    active: Mutex<Option<ActiveAutoScan>>,
    tombstone: Mutex<Option<AutoScanStatusDto>>,
    generations: AtomicU64,
}

impl AutoScanController {
    pub(crate) fn start(
        &self,
        app: tauri::AppHandle,
        run_state: Arc<RunState>,
        binding: AutoScanBinding,
        local_roots: Option<(PathBuf, PathBuf)>,
    ) -> AutoScanStatusDto {
        let _gate = self.gate.lock().unwrap();
        self.stop_locked("AutoScan was rearmed");

        let generation = self.generations.fetch_add(1, Ordering::Relaxed) + 1;
        let initial = AutoScanStatusDto::starting(generation, &binding);
        let shared = Arc::new(Mutex::new(AutoScanShared {
            status: initial.clone(),
            pending_compare_min_run_id: None,
            pending_compare_owner: None,
            auto_apply_ticket: None,
        }));
        let (commands, receiver) = mpsc::channel();
        let worker_shared = shared.clone();
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
                    worker_shared,
                    run_state,
                );
            })
            .expect("AutoScan worker thread creation failed");
        *self.active.lock().unwrap() = Some(ActiveAutoScan {
            binding,
            generation,
            commands,
            shared,
            join: Some(join),
        });
        initial
    }

    pub(crate) fn stop(&self) -> AutoScanStatusDto {
        let _gate = self.gate.lock().unwrap();
        self.stop_locked("AutoScan is off")
            .or_else(|| self.tombstone.lock().unwrap().clone())
            .unwrap_or_else(AutoScanStatusDto::inactive)
    }

    fn stop_locked(&self, detail: &str) -> Option<AutoScanStatusDto> {
        let mut active_guard = self.active.lock().unwrap();
        let active = active_guard.as_ref()?;
        let snapshot = {
            let mut shared = active.shared.lock().unwrap();
            mark_shared_inactive(&mut shared, detail)
        };
        *self.tombstone.lock().unwrap() = Some(snapshot.clone());
        let mut active = active_guard
            .take()
            .expect("the tombstoned AutoScan generation must still be active");
        drop(active_guard);
        let _ = active.commands.send(WorkerCommand::Stop);
        if let Some(join) = active.join.take() {
            let _ = join.join();
        }
        Some(snapshot)
    }

    pub(crate) fn stop_if_job_id(&self, job_id: &str) -> Option<AutoScanStatusDto> {
        let _gate = self.gate.lock().unwrap();
        let matches = self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|active| active.binding.job_id == job_id);
        if matches {
            self.stop_locked("AutoScan stopped because its job changed")
        } else {
            None
        }
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
        let mut shared = active.shared.lock().ok()?;
        shared.status.job_name = Some(job_name.to_string());
        if let Some(trigger) = &mut shared.status.pending_trigger {
            trigger.job_name = job_name.to_string();
        }
        if let Some(owner) = &mut shared.pending_compare_owner {
            owner.job_name = job_name.to_string();
        }
        if let Some(record) = &mut shared.auto_apply_ticket {
            record.ticket.rebind_name(job_name);
        }
        Some(shared.status.clone())
    }

    pub(crate) fn status(&self) -> AutoScanStatusDto {
        let active = self.active.lock().unwrap().as_ref().and_then(|active| {
            active
                .shared
                .lock()
                .ok()
                .map(|shared| shared.status.clone())
        });
        active
            .or_else(|| self.tombstone.lock().unwrap().clone())
            .unwrap_or_else(AutoScanStatusDto::inactive)
    }

    pub(crate) fn complete(
        &self,
        generation: u64,
        ticket_id: u64,
        succeeded: bool,
        owner: Option<&CompareOwner>,
    ) -> Result<AutoScanStatusDto, String> {
        let _gate = self.gate.lock().unwrap();
        let active_guard = self.active.lock().unwrap();
        let active = active_guard
            .as_ref()
            .ok_or_else(|| "AutoScan is no longer active".to_string())?;
        if active.generation != generation {
            return Err("This AutoScan generation is no longer active".into());
        }
        let supplied_owner = if succeeded {
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
            Some(owner)
        } else {
            None
        };
        let mut shared = active
            .shared
            .lock()
            .map_err(|_| "AutoScan status lock is poisoned".to_string())?;
        let pending = shared
            .status
            .pending_trigger
            .as_ref()
            .filter(|trigger| {
                trigger.generation == generation
                    && trigger.ticket_id == ticket_id
                    && trigger.job_id == active.binding.job_id
                    && trigger.config_revision == active.binding.config_revision
                    && trigger.target_index == active.binding.target_index
            })
            .cloned()
            .ok_or_else(|| {
                "This AutoScan work ticket is no longer awaiting completion".to_string()
            })?;
        if shared.status.active_ticket != Some(ticket_id) {
            return Err("This AutoScan work ticket is no longer awaiting completion".into());
        }
        let completed_owner = match supplied_owner {
            Some(supplied) => {
                let recorded = shared.pending_compare_owner.as_ref().ok_or_else(|| {
                    "This AutoScan ticket has no successful server-recorded Compare result"
                        .to_string()
                })?;
                if recorded.identity != supplied.identity {
                    return Err(
                        "The supplied Compare result was not produced for this AutoScan ticket"
                            .into(),
                    );
                }
                Some(recorded.clone())
            }
            None => None,
        };
        active
            .commands
            .send(WorkerCommand::Complete {
                ticket_id,
                succeeded,
            })
            .map_err(|_| "The AutoScan worker has stopped".to_string())?;
        // Commit status and AutoApply ownership under one lock. A duplicate completion cannot
        // observe the pending trigger, and a status query can recover either side of this edge.
        shared.status.active_ticket = None;
        shared.status.pending_trigger = None;
        shared.pending_compare_min_run_id = None;
        shared.pending_compare_owner = None;
        shared.auto_apply_ticket = None;
        if let (true, true, Some(mut owner)) =
            (succeeded, active.binding.auto_apply, completed_owner)
        {
            owner.job_name = pending.job_name.clone();
            shared.auto_apply_ticket = Some(AutoApplyTicketRecord {
                stage: AutoApplyTicketStage::Completed,
                ticket: AutoApplyTicket {
                    generation,
                    ticket_id,
                    job_id: active.binding.job_id.clone(),
                    job_name: pending.job_name,
                    config_revision: active.binding.config_revision.clone(),
                    target_index: active.binding.target_index,
                    owner,
                },
            });
        }
        shared.status.detail = if succeeded {
            "Verification complete; waiting for changes".into()
        } else {
            "Verification did not complete; waiting to retry".into()
        };
        Ok(shared.status.clone())
    }

    /// Associate a freshly cached successful Compare with the currently pending trigger. This is
    /// the server-side bridge that prevents a caller from promoting an older cached owner by merely
    /// supplying its public Compare run ID to `complete_autoscan`.
    pub(crate) fn record_successful_compare(&self, owner: &CompareOwner) -> bool {
        let _gate = self.gate.lock().unwrap();
        let active_guard = self.active.lock().unwrap();
        let Some(active) = active_guard
            .as_ref()
            .filter(|active| active.binding.owns_compare(owner))
        else {
            return false;
        };
        let Ok(mut shared) = active.shared.lock() else {
            return false;
        };
        let Some(pending) = shared.status.pending_trigger.as_ref() else {
            return false;
        };
        let Some(minimum_compare_run_id) = shared.pending_compare_min_run_id else {
            return false;
        };
        if shared.status.active_ticket != Some(pending.ticket_id)
            || pending.generation != active.generation
            || pending.job_id != owner.identity.job_id
            || pending.config_revision != owner.identity.config_revision
            || pending.target_index != owner.identity.target_index
            || owner.identity.compare_run_id < minimum_compare_run_id
        {
            return false;
        }
        // Compare reloads the registry immediately before caching, so its label is fresher than the
        // trigger's display copy after an external pure rename. Relabel all presentation state while
        // retaining the exact identity/revision/target and ticket cursor.
        shared.status.job_name = Some(owner.job_name.clone());
        shared
            .status
            .pending_trigger
            .as_mut()
            .expect("the validated pending trigger must still exist")
            .job_name = owner.job_name.clone();
        shared.pending_compare_owner = Some(owner.clone());
        true
    }

    /// Move one exact completed ticket into a non-retryable claimed state. Endpoint probes happen
    /// after this method returns, so no AutoScan lock is held across network or filesystem I/O.
    pub(crate) fn claim_completed_auto_apply(
        &self,
        generation: u64,
        ticket_id: u64,
    ) -> Result<AutoApplyTicket, String> {
        let _gate = self.gate.lock().unwrap();
        let active_guard = self.active.lock().unwrap();
        let active = active_guard
            .as_ref()
            .ok_or_else(|| "AutoScan is no longer active".to_string())?;
        let mut shared = active
            .shared
            .lock()
            .map_err(|_| "AutoScan status lock is poisoned".to_string())?;
        let record = shared.auto_apply_ticket.as_mut().ok_or_else(|| {
            "This AutoScan ticket has no completed AutoApply result to claim".to_string()
        })?;
        if record.stage != AutoApplyTicketStage::Completed
            || !record.ticket.matches_key(generation, ticket_id)
            || !active_owns_ticket(active, &record.ticket)
        {
            return Err("This AutoScan AutoApply ticket is stale or was already used".into());
        }
        record.stage = AutoApplyTicketStage::Claimed;
        Ok(record.ticket.clone())
    }

    /// Mint authority only while the claimed ticket is still the active generation's latest work.
    /// `issue` may lock the authorization store but performs no endpoint I/O.
    pub(crate) fn authorize_claim<T>(
        &self,
        ticket: &AutoApplyTicket,
        issue: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = self.gate.lock().unwrap();
        let active_guard = self.active.lock().unwrap();
        let active = active_guard
            .as_ref()
            .filter(|active| active_owns_ticket(active, ticket))
            .ok_or_else(|| {
                "This AutoScan generation stopped or changed before authorization".to_string()
            })?;
        let mut shared = active
            .shared
            .lock()
            .map_err(|_| "AutoScan status lock is poisoned".to_string())?;
        let record = shared
            .auto_apply_ticket
            .as_ref()
            .ok_or_else(|| "This AutoScan AutoApply claim is no longer active".to_string())?;
        if record.stage != AutoApplyTicketStage::Claimed || !record.ticket.same_authority(ticket) {
            return Err("This AutoScan AutoApply claim is stale or was already used".into());
        }
        match issue() {
            Ok(value) => {
                shared
                    .auto_apply_ticket
                    .as_mut()
                    .expect("the validated claim must still exist")
                    .stage = AutoApplyTicketStage::Authorized;
                Ok(value)
            }
            Err(error) => {
                // The claim itself is one-use. A failed grant lookup or token mint cannot be retried
                // by retaining an internal ticket value after the public claim edge has closed.
                shared.auto_apply_ticket = None;
                Err(error)
            }
        }
    }

    /// Consume one authorized ticket at the final registry/result recheck and run reservation.
    /// Holding the short state lock through `reserve` prevents stop/rearm/new-trigger from crossing
    /// the exact transition into an active Apply; `reserve` must not perform endpoint probes.
    pub(crate) fn consume_authorized_with<T>(
        &self,
        ticket: &AutoApplyTicket,
        reserve: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = self.gate.lock().unwrap();
        let active_guard = self.active.lock().unwrap();
        let active = active_guard
            .as_ref()
            .filter(|active| active_owns_ticket(active, ticket))
            .ok_or_else(|| {
                "This AutoScan generation stopped or changed before Apply".to_string()
            })?;
        let mut shared = active
            .shared
            .lock()
            .map_err(|_| "AutoScan status lock is poisoned".to_string())?;
        let record = shared.auto_apply_ticket.as_ref().ok_or_else(|| {
            "This AutoScan AutoApply authorization is no longer active".to_string()
        })?;
        if record.stage != AutoApplyTicketStage::Authorized || !record.ticket.same_authority(ticket)
        {
            return Err("This AutoScan AutoApply authorization is stale or already used".into());
        }
        shared.auto_apply_ticket = None;
        reserve()
    }
}

fn active_owns_ticket(active: &ActiveAutoScan, ticket: &AutoApplyTicket) -> bool {
    active.generation == ticket.generation
        && active.binding.auto_apply
        && active.binding.job_id == ticket.job_id
        && active.binding.config_revision == ticket.config_revision
        && active.binding.target_index == ticket.target_index
        && active.binding.owns_compare(&ticket.owner)
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

fn mark_shared_inactive(
    shared: &mut AutoScanShared,
    detail: impl Into<String>,
) -> AutoScanStatusDto {
    // Preserve generation, ticket cursor, and bound display identity as an orderable tombstone.
    // Only the never-started controller uses the all-zero inactive snapshot.
    shared.status.active = false;
    shared.status.active_ticket = None;
    shared.status.pending_trigger = None;
    shared.status.detail = detail.into();
    shared.pending_compare_min_run_id = None;
    shared.pending_compare_owner = None;
    shared.auto_apply_ticket = None;
    shared.status.clone()
}

fn publish_status(
    app: &tauri::AppHandle,
    shared: &Arc<Mutex<AutoScanShared>>,
    mode: AutoScanMode,
    detail: impl Into<String>,
) {
    let snapshot = {
        let mut shared = shared.lock().unwrap();
        shared.status.mode = Some(mode);
        shared.status.detail = detail.into();
        shared.status.clone()
    };
    let _ = app.emit("autoscan-status", snapshot);
}

#[allow(clippy::too_many_arguments)]
fn publish_trigger(
    app: &tauri::AppHandle,
    shared: &Arc<Mutex<AutoScanShared>>,
    generation: u64,
    binding: &AutoScanBinding,
    run_state: &RunState,
    ticket_id: u64,
    mode: AutoScanMode,
    reason: AutoScanReason,
) -> bool {
    let job_name = match resolve_binding_job_name(binding) {
        Ok(job_name) => job_name,
        Err(error) => {
            let snapshot = {
                let mut shared = shared.lock().unwrap();
                mark_shared_inactive(&mut shared, format!("AutoScan stopped safely: {error}"))
            };
            let _ = app.emit("autoscan-status", snapshot);
            return false;
        }
    };
    let trigger = AutoScanTriggerDto {
        generation,
        ticket_id,
        job_id: binding.job_id.clone(),
        job_name,
        config_revision: binding.config_revision.clone(),
        target_index: binding.target_index,
        auto_apply: binding.auto_apply,
        mode,
        reason,
    };
    {
        let mut shared = shared.lock().unwrap();
        // New work supersedes any completed, claimed, or authorized predecessor before the event
        // becomes observable. A missed event is recoverable from this exact status snapshot.
        shared.auto_apply_ticket = None;
        // A zero-delta RMW linearizes against run-id allocation. A Compare reserved before this
        // trigger can still complete interactively, but cannot be registered as this ticket's work.
        shared.pending_compare_min_run_id = Some(
            run_state
                .seq
                .fetch_add(0, Ordering::AcqRel)
                .saturating_add(1),
        );
        shared.pending_compare_owner = None;
        shared.status.latest_ticket_id = ticket_id;
        shared.status.active_ticket = Some(ticket_id);
        shared.status.pending_trigger = Some(trigger.clone());
        shared.status.job_name = Some(trigger.job_name.clone());
        shared.status.mode = Some(mode);
        shared.status.detail = match reason {
            AutoScanReason::Bootstrap => "Running the initial verification".into(),
            AutoScanReason::FilesystemChange => "A filesystem change requested verification".into(),
            AutoScanReason::WatchInvalidated => {
                "The native event history changed; a full verification is required".into()
            }
            AutoScanReason::PeriodicVerification => "Running the periodic full verification".into(),
        };
    }
    let _ = app.emit("autoscan-trigger", trigger);
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
    shared: Arc<Mutex<AutoScanShared>>,
    run_state: Arc<RunState>,
) {
    #[cfg(target_os = "macos")]
    if let Some((source, target)) = local_roots {
        match run_native_macos(
            app, generation, &binding, &source, &target, &commands, &shared, &run_state,
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
                    &shared,
                    &run_state,
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
        &shared,
        &run_state,
        fallback.into(),
        1,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn run_polling(
    app: &tauri::AppHandle,
    generation: u64,
    binding: &AutoScanBinding,
    commands: &mpsc::Receiver<WorkerCommand>,
    shared: &Arc<Mutex<AutoScanShared>>,
    run_state: &RunState,
    detail: String,
    mut next_ticket: u64,
    immediate: bool,
) {
    publish_status(app, shared, AutoScanMode::Polling, detail);
    let interval = binding.interval();
    let mut deadline = if immediate {
        Instant::now()
    } else {
        Instant::now() + interval
    };
    let mut awaiting = shared.lock().unwrap().status.active_ticket;
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
                shared,
                generation,
                binding,
                run_state,
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
#[allow(clippy::too_many_arguments)]
fn run_native_macos(
    app: &tauri::AppHandle,
    generation: u64,
    binding: &AutoScanBinding,
    source: &std::path::Path,
    target: &std::path::Path,
    commands: &mpsc::Receiver<WorkerCommand>,
    shared: &Arc<Mutex<AutoScanShared>>,
    run_state: &RunState,
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
        shared,
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
            shared,
            generation,
            binding,
            run_state,
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
    use std::sync::Barrier;

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
            identity: crate::dto::CompareIdentity {
                compare_run_id: 7,
                job_id: "job-id-photos".into(),
                config_revision: "revision-a".into(),
                target_index: 1,
            },
            job_name: "photos".into(),
        };
        assert!(binding.owns_compare(&owner));
        assert!(binding.owns_compare(&CompareOwner {
            job_name: "renamed".into(),
            ..owner.clone()
        }));
        let mut replaced = owner.clone();
        replaced.identity.job_id = "replacement-id".into();
        assert!(!binding.owns_compare(&replaced));
        let mut revised = owner.clone();
        revised.identity.config_revision = "revision-b".into();
        assert!(!binding.owns_compare(&revised));
        let mut retargeted = owner;
        retargeted.identity.target_index = 0;
        assert!(!binding.owns_compare(&retargeted));
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

    fn trigger(binding: &AutoScanBinding, generation: u64, ticket_id: u64) -> AutoScanTriggerDto {
        AutoScanTriggerDto {
            generation,
            ticket_id,
            job_id: binding.job_id.clone(),
            job_name: binding.job_name.clone(),
            config_revision: binding.config_revision.clone(),
            target_index: binding.target_index,
            auto_apply: binding.auto_apply,
            mode: AutoScanMode::Polling,
            reason: AutoScanReason::FilesystemChange,
        }
    }

    fn controller_waiting_for(
        ticket_id: u64,
        auto_apply: bool,
    ) -> (AutoScanController, mpsc::Receiver<WorkerCommand>) {
        let controller = AutoScanController::default();
        let mut binding = binding();
        binding.auto_apply = auto_apply;
        let (commands, receiver) = mpsc::channel();
        let mut status = AutoScanStatusDto::starting(4, &binding);
        status.latest_ticket_id = ticket_id;
        status.active_ticket = Some(ticket_id);
        status.pending_trigger = Some(trigger(&binding, 4, ticket_id));
        *controller.active.lock().unwrap() = Some(ActiveAutoScan {
            binding,
            generation: 4,
            commands,
            shared: Arc::new(Mutex::new(AutoScanShared {
                status,
                pending_compare_min_run_id: Some(1),
                pending_compare_owner: None,
                auto_apply_ticket: None,
            })),
            join: None,
        });
        (controller, receiver)
    }

    fn owner() -> CompareOwner {
        CompareOwner {
            identity: crate::dto::CompareIdentity {
                compare_run_id: 8,
                job_id: "job-id-photos".into(),
                config_revision: "revision-a".into(),
                target_index: 1,
            },
            job_name: "photos".into(),
        }
    }

    fn record_current_compare(controller: &AutoScanController) {
        assert!(controller.record_successful_compare(&owner()));
    }

    #[test]
    fn completion_is_one_use_and_requires_the_exact_generation_and_ticket() {
        let (controller, receiver) = controller_waiting_for(11, false);
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
        let (controller, receiver) = controller_waiting_for(12, false);
        let mut wrong = owner();
        wrong.identity.target_index = 0;
        assert!(controller.complete(4, 12, true, None).is_err());
        assert!(controller.complete(4, 12, true, Some(&wrong)).is_err());
        assert!(receiver.try_recv().is_err());
        record_current_compare(&controller);
        let mut older = owner();
        older.identity.compare_run_id -= 1;
        assert!(controller.complete(4, 12, true, Some(&older)).is_err());
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
    fn compare_reserved_before_the_trigger_cannot_satisfy_the_ticket() {
        let (controller, _receiver) = controller_waiting_for(24, false);
        controller
            .active
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .shared
            .lock()
            .unwrap()
            .pending_compare_min_run_id = Some(9);

        assert!(!controller.record_successful_compare(&owner()));
        let mut current = owner();
        current.identity.compare_run_id = 9;
        assert!(controller.record_successful_compare(&current));
        assert!(controller.complete(4, 24, true, Some(&owner())).is_err());
        assert!(controller.complete(4, 24, true, Some(&current)).is_ok());
    }

    #[test]
    fn successful_compare_refreshes_a_renamed_display_label_without_changing_authority() {
        let (controller, _receiver) = controller_waiting_for(25, true);
        let mut renamed = owner();
        renamed.job_name = "externally-renamed".into();
        assert!(controller.record_successful_compare(&renamed));
        let status = controller.status();
        assert_eq!(status.job_name.as_deref(), Some("externally-renamed"));
        assert_eq!(
            status
                .pending_trigger
                .as_ref()
                .map(|item| item.job_name.as_str()),
            Some("externally-renamed")
        );

        // Display names are deliberately excluded from authority equality.
        controller.complete(4, 25, true, Some(&owner())).unwrap();
        let ticket = controller.claim_completed_auto_apply(4, 25).unwrap();
        assert_eq!(ticket.job_name, "externally-renamed");
        assert_eq!(ticket.owner.job_name, "externally-renamed");
    }

    #[test]
    fn status_query_recovers_the_full_trigger_when_event_delivery_is_lost() {
        let (controller, _receiver) = controller_waiting_for(14, true);
        let status = controller.status();
        let pending = status
            .pending_trigger
            .expect("the server-owned trigger must survive independently of event delivery");
        assert_eq!(status.latest_ticket_id, 14);
        assert_eq!(pending.generation, 4);
        assert_eq!(pending.ticket_id, 14);
        assert_eq!(pending.job_id, "job-id-photos");
        assert_eq!(pending.config_revision, "revision-a");
        assert_eq!(pending.target_index, 1);
        assert!(pending.auto_apply);
    }

    #[test]
    fn completed_autoapply_ticket_is_claimed_authorized_and_consumed_exactly_once() {
        let (controller, _receiver) = controller_waiting_for(15, true);
        record_current_compare(&controller);
        let status = controller.complete(4, 15, true, Some(&owner())).unwrap();
        assert_eq!(status.latest_ticket_id, 15);
        assert_eq!(status.active_ticket, None);
        assert_eq!(status.pending_trigger, None);

        assert!(controller.claim_completed_auto_apply(3, 15).is_err());
        assert!(controller.claim_completed_auto_apply(4, 14).is_err());
        let ticket = controller.claim_completed_auto_apply(4, 15).unwrap();
        assert!(controller.claim_completed_auto_apply(4, 15).is_err());
        assert_eq!(
            controller
                .authorize_claim(&ticket, || Ok::<_, String>("token"))
                .unwrap(),
            "token"
        );
        assert_eq!(
            controller
                .consume_authorized_with(&ticket, || Ok::<_, String>("reserved"))
                .unwrap(),
            "reserved"
        );
        assert!(controller
            .consume_authorized_with(&ticket, || Ok::<_, String>(()))
            .is_err());
    }

    #[test]
    fn completed_autoapply_claim_has_exactly_one_concurrent_winner() {
        let (controller, _receiver) = controller_waiting_for(20, true);
        record_current_compare(&controller);
        controller.complete(4, 20, true, Some(&owner())).unwrap();
        let controller = Arc::new(controller);
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let controller = controller.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                controller.claim_completed_auto_apply(4, 20).is_ok()
            }));
        }
        barrier.wait();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
    }

    #[test]
    fn failed_authorization_or_reservation_cannot_replay_the_ticket() {
        let (controller, _receiver) = controller_waiting_for(21, true);
        record_current_compare(&controller);
        controller.complete(4, 21, true, Some(&owner())).unwrap();
        let ticket = controller.claim_completed_auto_apply(4, 21).unwrap();
        assert!(controller
            .authorize_claim(&ticket, || Err::<(), _>("grant disappeared".into()))
            .is_err());
        assert!(controller.claim_completed_auto_apply(4, 21).is_err());
        assert!(controller
            .authorize_claim(&ticket, || Ok::<_, String>(()))
            .is_err());

        let (controller, _receiver) = controller_waiting_for(22, true);
        record_current_compare(&controller);
        controller.complete(4, 22, true, Some(&owner())).unwrap();
        let ticket = controller.claim_completed_auto_apply(4, 22).unwrap();
        controller
            .authorize_claim(&ticket, || Ok::<_, String>(()))
            .unwrap();
        assert!(controller
            .consume_authorized_with(&ticket, || Err::<(), _>("reservation lost".into()))
            .is_err());
        assert!(controller
            .consume_authorized_with(&ticket, || Ok::<_, String>(()))
            .is_err());
    }

    #[test]
    fn failed_or_non_autoapply_completion_never_creates_write_authority() {
        let (failed, _receiver) = controller_waiting_for(16, true);
        failed.complete(4, 16, false, None).unwrap();
        assert!(failed.claim_completed_auto_apply(4, 16).is_err());

        let (manual, _receiver) = controller_waiting_for(17, false);
        record_current_compare(&manual);
        manual.complete(4, 17, true, Some(&owner())).unwrap();
        assert!(manual.claim_completed_auto_apply(4, 17).is_err());
    }

    #[test]
    fn stop_discards_a_claim_and_rename_relabels_without_invalidating_it() {
        let (controller, _receiver) = controller_waiting_for(18, true);
        record_current_compare(&controller);
        controller
            .rebind_job_name("job-id-photos", "renamed-before-completion")
            .unwrap();
        controller.complete(4, 18, true, Some(&owner())).unwrap();
        let _ = controller
            .rebind_job_name("job-id-photos", "renamed-after-completion")
            .unwrap();
        let ticket = controller.claim_completed_auto_apply(4, 18).unwrap();
        assert_eq!(ticket.job_name, "renamed-after-completion");
        assert_eq!(ticket.owner.job_name, "renamed-after-completion");
        let stopped = controller.stop();
        assert!(!stopped.active);
        assert_eq!(stopped.generation, 4);
        assert_eq!(stopped.latest_ticket_id, 18);
        assert_eq!(controller.status(), stopped);
        assert!(controller
            .authorize_claim(&ticket, || Ok::<_, String>(()))
            .is_err());
    }

    #[test]
    fn job_mutation_stops_only_the_monitored_identity_and_reports_canonical_inactive() {
        let never_started = AutoScanController::default().status();
        assert!(!never_started.active);
        assert_eq!(never_started.generation, 0);
        assert_eq!(never_started.latest_ticket_id, 0);

        let (controller, _receiver) = controller_waiting_for(23, true);
        assert!(controller.stop_if_job_id("unrelated-job").is_none());
        assert!(controller.status().active);
        assert_eq!(controller.status().active_ticket, Some(23));

        let inactive = controller
            .stop_if_job_id("job-id-photos")
            .expect("the monitored identity must be stopped");
        assert!(!inactive.active);
        assert_eq!(inactive.generation, 4);
        assert_eq!(inactive.latest_ticket_id, 23);
        assert_eq!(inactive.job_id.as_deref(), Some("job-id-photos"));
        assert_eq!(inactive.pending_trigger, None);
        assert_eq!(controller.status(), inactive);
    }

    #[test]
    fn worker_failure_retains_an_orderable_recoverable_tombstone() {
        let (controller, _receiver) = controller_waiting_for(26, true);
        let shared = controller
            .active
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .shared
            .clone();
        let stopped = mark_shared_inactive(&mut shared.lock().unwrap(), "registry changed");
        assert!(!stopped.active);
        assert_eq!(stopped.generation, 4);
        assert_eq!(stopped.latest_ticket_id, 26);
        assert_eq!(stopped.job_id.as_deref(), Some("job-id-photos"));
        assert_eq!(stopped.active_ticket, None);
        assert_eq!(stopped.pending_trigger, None);
        assert_eq!(controller.status(), stopped);
    }

    #[test]
    fn concurrent_duplicate_completions_have_exactly_one_winner() {
        let (controller, receiver) = controller_waiting_for(19, false);
        let controller = Arc::new(controller);
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let controller = controller.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                controller.complete(4, 19, false, None).is_ok()
            }));
        }
        barrier.wait();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::Complete { ticket_id: 19, .. })
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn pure_rename_rebinds_status_without_consuming_generation_or_ticket() {
        let (controller, _receiver) = controller_waiting_for(13, false);
        assert!(controller
            .rebind_job_name("replacement-id", "ignored")
            .is_none());

        let status = controller
            .rebind_job_name("job-id-photos", "renamed")
            .expect("the active identity should be rebound");
        assert_eq!(status.generation, 4);
        assert_eq!(status.latest_ticket_id, 13);
        assert_eq!(status.active_ticket, Some(13));
        assert_eq!(status.job_id.as_deref(), Some("job-id-photos"));
        assert_eq!(status.job_name.as_deref(), Some("renamed"));
        assert_eq!(
            status
                .pending_trigger
                .as_ref()
                .map(|trigger| trigger.job_name.as_str()),
            Some("renamed")
        );
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
        let mut registered = syncdash::job::Job {
            job_id: "job-id-original".into(),
            ..Default::default()
        };
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
