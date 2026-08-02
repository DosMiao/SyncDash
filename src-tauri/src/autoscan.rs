//! Backend-owned AutoScan lifecycle.
//!
//! The webview is a subscriber, never the clock. One generation owns one exact job identity,
//! revision and target. Local macOS roots use FSEvents as a trigger plus a periodic full verification;
//! roots without local event support and unsupported platforms use interval polling explicitly.
//! Repository freshness and the worker ticket share one terminalization edge, so review, launch,
//! Compare, stop, and binding failures cannot leave execution eligibility awaiting indefinitely.
//! Successful evidence publication completes worker ownership directly; the webview never confirms it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::Emitter;

use crate::compare_results::{
    CompareResultRepository, CompareScope, CompareVerificationTerminalOutcome,
    CompareVerificationTicket, SuccessfulComparePublication, SuccessfulCompareResult,
};
use crate::dto::{CompareIdentity, CompareOwner, CompareScopeExecutionStatusDto};
use crate::operation_authorization::OperationAuthorizationStore;
use crate::window_role::MAIN_WINDOW_LABEL;

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

    fn compare_scope(&self) -> CompareScope {
        CompareScope::new(&self.job_id, self.target_index, &self.config_revision)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum AutoScanStatusMode {
    Starting,
    NativeFsevents,
    Polling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum AutoScanDetectionMode {
    NativeFsevents,
    Polling,
}

impl From<AutoScanDetectionMode> for AutoScanStatusMode {
    fn from(mode: AutoScanDetectionMode) -> Self {
        match mode {
            AutoScanDetectionMode::NativeFsevents => Self::NativeFsevents,
            AutoScanDetectionMode::Polling => Self::Polling,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum AutoScanTriggerReason {
    Bootstrap,
    FilesystemChange,
    WatchInvalidated,
    PeriodicVerification,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct AutoScanStatusDto {
    pub(crate) active: bool,
    #[ts(type = "number")]
    pub(crate) generation: u64,
    pub(crate) job_id: Option<String>,
    pub(crate) job_name: Option<String>,
    pub(crate) config_revision: Option<String>,
    #[ts(type = "number | null")]
    pub(crate) target_index: Option<usize>,
    #[ts(type = "number | null")]
    pub(crate) interval_secs: Option<u64>,
    pub(crate) auto_apply: bool,
    pub(crate) mode: Option<AutoScanStatusMode>,
    pub(crate) detail: String,
    /// Monotonic within one generation and retained after completion, so delayed same-generation
    /// IPC snapshots cannot make older ticket state appear current.
    #[ts(type = "number")]
    pub(crate) latest_ticket_id: u64,
    #[ts(type = "number | null")]
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
}

#[derive(Clone, Debug)]
struct AutoScanStatusCore {
    active: bool,
    generation: u64,
    job_id: Option<String>,
    job_name: Option<String>,
    config_revision: Option<String>,
    target_index: Option<usize>,
    interval_secs: Option<u64>,
    auto_apply: bool,
    mode: Option<AutoScanStatusMode>,
    detail: String,
    latest_ticket_id: u64,
}

impl AutoScanStatusCore {
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
            mode: Some(AutoScanStatusMode::Starting),
            detail: "Preparing backend-owned change detection".into(),
            latest_ticket_id: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct AutoScanTriggerDto {
    #[ts(type = "number")]
    pub(crate) generation: u64,
    #[ts(type = "number")]
    pub(crate) ticket_id: u64,
    pub(crate) job_id: String,
    pub(crate) job_name: String,
    pub(crate) config_revision: String,
    #[ts(type = "number")]
    pub(crate) target_index: usize,
    pub(crate) auto_apply: bool,
    pub(crate) mode: AutoScanDetectionMode,
    pub(crate) reason: AutoScanTriggerReason,
}

/// One-use authority to associate one Compare launch with one exact AutoScan trigger. The permit
/// is issued before authorization and must travel inside that Compare authorization until the
/// successful result is registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutoScanComparePermit {
    permit_id: u64,
    generation: u64,
    ticket_id: u64,
    job_id: String,
    config_revision: String,
    target_index: usize,
    verification: CompareVerificationTicket,
}

impl AutoScanComparePermit {
    fn owns_compare(&self, owner: &CompareOwner) -> bool {
        owner.identity.job_id == self.job_id
            && owner.identity.config_revision == self.config_revision
            && owner.identity.target_index == self.target_index
    }

    pub(crate) fn verification(&self) -> &CompareVerificationTicket {
        &self.verification
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AutoScanVerificationTerminal {
    Failed(String),
    Cancelled,
}

impl AutoScanVerificationTerminal {
    fn repository_outcome(&self) -> CompareVerificationTerminalOutcome {
        match self {
            Self::Failed(message) => CompareVerificationTerminalOutcome::Failed {
                message: format!("AutoScan verification failed: {message}"),
            },
            Self::Cancelled => CompareVerificationTerminalOutcome::Cancelled,
        }
    }

    fn status_detail(&self) -> String {
        match self {
            Self::Failed(message) => format!("Verification failed: {message}"),
            Self::Cancelled => "Verification was cancelled; waiting for changes".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutoApplyTicket {
    generation: u64,
    ticket_id: u64,
    compare_identity: CompareIdentity,
}

impl AutoApplyTicket {
    fn matches_key(&self, generation: u64, ticket_id: u64) -> bool {
        self.generation == generation && self.ticket_id == ticket_id
    }

    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        self == other
    }

    pub(crate) fn compare_identity(&self) -> &CompareIdentity {
        &self.compare_identity
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        generation: u64,
        ticket_id: u64,
        compare_identity: CompareIdentity,
    ) -> Self {
        Self {
            generation,
            ticket_id,
            compare_identity,
        }
    }
}

#[derive(Clone, Debug)]
enum AutoScanTicketLifecycle {
    Idle,
    AwaitingCompare {
        trigger: AutoScanTriggerDto,
        verification: CompareVerificationTicket,
    },
    ComparePermitted {
        trigger: AutoScanTriggerDto,
        permit: AutoScanComparePermit,
    },
    CompareRunning {
        trigger: AutoScanTriggerDto,
        permit: AutoScanComparePermit,
    },
    AutoApplyCompleted {
        ticket: AutoApplyTicket,
    },
    AutoApplyClaimed {
        ticket: AutoApplyTicket,
    },
    AutoApplyAuthorized {
        ticket: AutoApplyTicket,
    },
}

impl AutoScanTicketLifecycle {
    fn pending_trigger(&self) -> Option<&AutoScanTriggerDto> {
        match self {
            Self::AwaitingCompare { trigger, .. }
            | Self::ComparePermitted { trigger, .. }
            | Self::CompareRunning { trigger, .. } => Some(trigger),
            Self::Idle
            | Self::AutoApplyCompleted { .. }
            | Self::AutoApplyClaimed { .. }
            | Self::AutoApplyAuthorized { .. } => None,
        }
    }

    fn rebind_job_name(&mut self, job_name: &str) {
        match self {
            Self::AwaitingCompare { trigger, .. }
            | Self::ComparePermitted { trigger, .. }
            | Self::CompareRunning { trigger, .. } => {
                trigger.job_name = job_name.to_string();
            }
            Self::Idle
            | Self::AutoApplyCompleted { .. }
            | Self::AutoApplyClaimed { .. }
            | Self::AutoApplyAuthorized { .. } => {}
        }
    }

    fn verification(&self) -> Option<&CompareVerificationTicket> {
        match self {
            Self::AwaitingCompare { verification, .. } => Some(verification),
            Self::ComparePermitted { permit, .. } | Self::CompareRunning { permit, .. } => {
                Some(permit.verification())
            }
            Self::Idle
            | Self::AutoApplyCompleted { .. }
            | Self::AutoApplyClaimed { .. }
            | Self::AutoApplyAuthorized { .. } => None,
        }
    }

    fn owns_permit(&self, expected: &AutoScanComparePermit) -> bool {
        matches!(
            self,
            Self::ComparePermitted { permit, .. } | Self::CompareRunning { permit, .. }
                if permit == expected
        )
    }

    fn can_be_declined(&self) -> bool {
        matches!(
            self,
            Self::AwaitingCompare { .. } | Self::ComparePermitted { .. }
        )
    }
}

struct AutoScanShared {
    status: AutoScanStatusCore,
    ticket: AutoScanTicketLifecycle,
}

impl AutoScanShared {
    fn snapshot(&self) -> AutoScanStatusDto {
        let pending_trigger = self.ticket.pending_trigger().cloned();
        AutoScanStatusDto {
            active: self.status.active,
            generation: self.status.generation,
            job_id: self.status.job_id.clone(),
            job_name: self.status.job_name.clone(),
            config_revision: self.status.config_revision.clone(),
            target_index: self.status.target_index,
            interval_secs: self.status.interval_secs,
            auto_apply: self.status.auto_apply,
            mode: self.status.mode,
            detail: self.status.detail.clone(),
            latest_ticket_id: self.status.latest_ticket_id,
            active_ticket: pending_trigger.as_ref().map(|trigger| trigger.ticket_id),
            pending_trigger,
        }
    }
}

fn allocate_unique_id(counter: &AtomicU64, identity: &str) -> Result<u64, String> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| format!("The {identity} ID space is exhausted — restart SyncDash"))
}

enum WorkerCommand {
    VerificationPublished { ticket_id: u64 },
    VerificationTerminated { ticket_id: u64 },
    Stop,
}

struct ActiveAutoScan {
    binding: AutoScanBinding,
    generation: u64,
    commands: mpsc::Sender<WorkerCommand>,
    shared: Arc<Mutex<AutoScanShared>>,
    events: AutoScanEvents,
    join: Option<JoinHandle<()>>,
}

#[derive(Clone)]
enum AutoScanEvents {
    Application(tauri::AppHandle),
    #[cfg(test)]
    Suppressed,
}

impl AutoScanEvents {
    fn emit_execution_transition(&self, status: Option<CompareScopeExecutionStatusDto>) {
        if let (Self::Application(app), Some(status)) = (self, status) {
            let _ = app.emit_to(MAIN_WINDOW_LABEL, "compare-execution-status", status);
        }
    }

    fn emit_status(&self, status: AutoScanStatusDto) {
        match self {
            Self::Application(app) => {
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", status);
            }
            #[cfg(test)]
            Self::Suppressed => {}
        }
    }
}

#[derive(Clone)]
struct AutoScanExecutionServices {
    results: Arc<CompareResultRepository>,
    authorizations: Arc<OperationAuthorizationStore>,
}

enum AutoScanTerminalAuthority<'a> {
    TriggerRequest,
    UnlaunchedTrigger,
    ComparePermit(&'a AutoScanComparePermit),
}

pub(crate) struct AutoScanComparePublication {
    pub(crate) publication: SuccessfulComparePublication,
    pub(crate) autoscan_status: AutoScanStatusDto,
}

pub(crate) struct AutoScanController {
    gate: Mutex<()>,
    active: Mutex<Option<ActiveAutoScan>>,
    tombstone: Mutex<Option<AutoScanStatusDto>>,
    generations: AtomicU64,
    compare_permits: AtomicU64,
    execution: AutoScanExecutionServices,
}

impl AutoScanController {
    pub(crate) fn new(
        results: Arc<CompareResultRepository>,
        authorizations: Arc<OperationAuthorizationStore>,
    ) -> Self {
        Self {
            gate: Mutex::new(()),
            active: Mutex::new(None),
            tombstone: Mutex::new(None),
            generations: AtomicU64::new(0),
            compare_permits: AtomicU64::new(0),
            execution: AutoScanExecutionServices {
                results,
                authorizations,
            },
        }
    }

    pub(crate) fn start(
        &self,
        app: tauri::AppHandle,
        binding: AutoScanBinding,
        local_roots: Option<(PathBuf, PathBuf)>,
    ) -> Result<AutoScanStatusDto, String> {
        let _gate = self.gate.lock().unwrap();
        let generation = allocate_unique_id(&self.generations, "AutoScan generation")?;
        self.stop_locked("AutoScan was rearmed");

        let status = AutoScanStatusCore::starting(generation, &binding);
        let shared = Arc::new(Mutex::new(AutoScanShared {
            status,
            ticket: AutoScanTicketLifecycle::Idle,
        }));
        let initial = shared.lock().unwrap().snapshot();
        let (commands, receiver) = mpsc::channel();
        let worker_shared = shared.clone();
        let worker_binding = binding.clone();
        let worker_execution = self.execution.clone();
        let events = AutoScanEvents::Application(app.clone());
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
                    worker_execution,
                );
            })
            .map_err(|error| format!("Could not start the AutoScan worker: {error}"))?;
        *self.active.lock().unwrap() = Some(ActiveAutoScan {
            binding,
            generation,
            commands,
            shared,
            events,
            join: Some(join),
        });
        Ok(initial)
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
        let (snapshot, execution_status, events) = {
            let mut shared = active.shared.lock().unwrap();
            let execution_status = shared.ticket.verification().and_then(|verification| {
                terminalize_repository_verification(
                    &self.execution,
                    &active.binding,
                    verification,
                    &AutoScanVerificationTerminal::Cancelled,
                )
            });
            (
                mark_shared_inactive(&mut shared, detail),
                execution_status,
                active.events.clone(),
            )
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
        events.emit_execution_transition(execution_status);
        events.emit_status(snapshot.clone());
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
        shared.ticket.rebind_job_name(job_name);
        Some(shared.snapshot())
    }

    pub(crate) fn status(&self) -> AutoScanStatusDto {
        let active = self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|active| active.shared.lock().ok().map(|shared| shared.snapshot()));
        active
            .or_else(|| self.tombstone.lock().unwrap().clone())
            .unwrap_or_else(AutoScanStatusDto::inactive)
    }

    pub(crate) fn decline_trigger(
        &self,
        generation: u64,
        ticket_id: u64,
    ) -> Result<AutoScanStatusDto, String> {
        self.terminalize_verification(
            generation,
            ticket_id,
            AutoScanVerificationTerminal::Failed(
                "The trigger was declined before Compare launched".into(),
            ),
            AutoScanTerminalAuthority::UnlaunchedTrigger,
        )
    }

    pub(crate) fn terminalize_verification_request(
        &self,
        generation: u64,
        ticket_id: u64,
        terminal: AutoScanVerificationTerminal,
    ) -> Result<AutoScanStatusDto, String> {
        self.terminalize_verification(
            generation,
            ticket_id,
            terminal,
            AutoScanTerminalAuthority::TriggerRequest,
        )
    }

    pub(crate) fn terminalize_permitted_verification(
        &self,
        permit: &AutoScanComparePermit,
        terminal: AutoScanVerificationTerminal,
    ) -> Result<AutoScanStatusDto, String> {
        self.terminalize_verification(
            permit.generation,
            permit.ticket_id,
            terminal,
            AutoScanTerminalAuthority::ComparePermit(permit),
        )
    }

    fn terminalize_verification(
        &self,
        generation: u64,
        ticket_id: u64,
        terminal: AutoScanVerificationTerminal,
        authority: AutoScanTerminalAuthority<'_>,
    ) -> Result<AutoScanStatusDto, String> {
        let _gate = self.gate.lock().unwrap();
        let active_guard = self.active.lock().unwrap();
        let active = active_guard
            .as_ref()
            .ok_or_else(|| "AutoScan is no longer active".to_string())?;
        if active.generation != generation {
            return Err("This AutoScan generation is no longer active".into());
        }
        let mut shared = active
            .shared
            .lock()
            .map_err(|_| "AutoScan status lock is poisoned".to_string())?;
        let pending_trigger = shared
            .ticket
            .pending_trigger()
            .filter(|trigger| {
                trigger.generation == generation
                    && trigger.ticket_id == ticket_id
                    && trigger.job_id == active.binding.job_id
                    && trigger.config_revision == active.binding.config_revision
                    && trigger.target_index == active.binding.target_index
            })
            .ok_or_else(|| {
                "This AutoScan work ticket is no longer awaiting completion".to_string()
            })?;
        match authority {
            AutoScanTerminalAuthority::TriggerRequest => {}
            AutoScanTerminalAuthority::UnlaunchedTrigger if shared.ticket.can_be_declined() => {}
            AutoScanTerminalAuthority::UnlaunchedTrigger => {
                return Err("This AutoScan trigger already launched Compare".into());
            }
            AutoScanTerminalAuthority::ComparePermit(expected)
                if shared.ticket.owns_permit(expected) => {}
            AutoScanTerminalAuthority::ComparePermit(_) => {
                return Err("This failure does not own the pending AutoScan Compare permit".into());
            }
        }
        debug_assert_eq!(pending_trigger.ticket_id, ticket_id);
        let repository_terminalized = shared.ticket.verification().and_then(|verification| {
            terminalize_repository_verification(
                &self.execution,
                &active.binding,
                verification,
                &terminal,
            )
        });
        shared.ticket = AutoScanTicketLifecycle::Idle;
        shared.status.detail = terminal.status_detail();
        let worker_stopped = active
            .commands
            .send(WorkerCommand::VerificationTerminated { ticket_id })
            .is_err();
        let repository_rejected = repository_terminalized.is_none();
        let execution_status = repository_terminalized.or_else(|| {
            Some(
                self.execution
                    .results
                    .execution_status(&active.binding.compare_scope()),
            )
        });
        let snapshot = if repository_rejected {
            mark_shared_inactive(
                &mut shared,
                "AutoScan stopped safely because its repository ticket was no longer current",
            )
        } else if worker_stopped {
            mark_shared_inactive(
                &mut shared,
                "AutoScan stopped safely because its worker disconnected during terminalization",
            )
        } else {
            shared.snapshot()
        };
        let events = active.events.clone();
        drop(shared);
        drop(active_guard);
        drop(_gate);
        if repository_rejected || worker_stopped {
            *self.tombstone.lock().unwrap() = Some(snapshot.clone());
        }
        events.emit_execution_transition(execution_status);
        events.emit_status(snapshot.clone());
        if repository_rejected {
            Err("The AutoScan repository ticket was no longer current".into())
        } else if worker_stopped {
            Err("The AutoScan worker has stopped".into())
        } else {
            Ok(snapshot)
        }
    }

    /// Issue one exact permit while a trigger is still pending. Manual Compare review never calls
    /// this method, so even a same-scope manual result cannot satisfy the trigger.
    pub(crate) fn issue_compare_permit(
        &self,
        generation: u64,
        ticket_id: u64,
    ) -> Result<AutoScanComparePermit, String> {
        let _gate = self.gate.lock().unwrap();
        let active_guard = self.active.lock().unwrap();
        let active = active_guard
            .as_ref()
            .filter(|active| active.generation == generation)
            .ok_or_else(|| "This AutoScan generation is no longer active".to_string())?;
        let mut shared = active
            .shared
            .lock()
            .map_err(|_| "AutoScan status lock is poisoned".to_string())?;
        let trigger_matches = |trigger: &AutoScanTriggerDto| {
            trigger.generation == generation
                && trigger.ticket_id == ticket_id
                && trigger.job_id == active.binding.job_id
                && trigger.config_revision == active.binding.config_revision
                && trigger.target_index == active.binding.target_index
        };
        let (pending, verification) = match &shared.ticket {
            AutoScanTicketLifecycle::AwaitingCompare {
                trigger,
                verification,
            } if trigger_matches(trigger) => (trigger.clone(), verification.clone()),
            AutoScanTicketLifecycle::ComparePermitted { trigger, permit }
                if trigger_matches(trigger) =>
            {
                return Ok(permit.clone());
            }
            AutoScanTicketLifecycle::CompareRunning { trigger, .. } if trigger_matches(trigger) => {
                return Err("This AutoScan work ticket already has a Compare launch".into());
            }
            _ => {
                return Err("This AutoScan work ticket is no longer awaiting Compare".into());
            }
        };
        let permit = AutoScanComparePermit {
            permit_id: allocate_unique_id(&self.compare_permits, "AutoScan Compare permit")?,
            generation: pending.generation,
            ticket_id: pending.ticket_id,
            job_id: pending.job_id.clone(),
            config_revision: pending.config_revision.clone(),
            target_index: pending.target_index,
            verification,
        };
        shared.ticket = AutoScanTicketLifecycle::ComparePermitted {
            trigger: pending,
            permit: permit.clone(),
        };
        Ok(permit)
    }

    pub(crate) fn mark_compare_launched(
        &self,
        permit: &AutoScanComparePermit,
        compare_run_id: u64,
    ) -> Result<(), String> {
        let _gate = self.gate.lock().unwrap();
        let active_guard = self.active.lock().unwrap();
        let active = active_guard
            .as_ref()
            .filter(|active| active.generation == permit.generation)
            .ok_or_else(|| "This AutoScan generation is no longer active".to_string())?;
        let mut shared = active
            .shared
            .lock()
            .map_err(|_| "AutoScan status lock is poisoned".to_string())?;
        let trigger = match &shared.ticket {
            AutoScanTicketLifecycle::ComparePermitted {
                trigger,
                permit: expected,
            } if expected == permit => trigger.clone(),
            AutoScanTicketLifecycle::CompareRunning {
                permit: expected, ..
            } if expected == permit => {
                return Err("This AutoScan Compare permit already launched a run".into());
            }
            _ => return Err("This Compare does not own the pending AutoScan permit".into()),
        };
        if !self
            .execution
            .results
            .mark_verification_comparing(permit.verification(), compare_run_id)
        {
            return Err("The AutoScan verification was superseded before Compare launched".into());
        }
        shared.ticket = AutoScanTicketLifecycle::CompareRunning {
            trigger,
            permit: permit.clone(),
        };
        Ok(())
    }

    /// Validate the exact running permit, publish its evidence, complete worker ownership, and
    /// expose one authoritative status transition while the stop/rearm gate excludes stale work.
    pub(crate) fn publish_successful_compare(
        &self,
        permit: &AutoScanComparePermit,
        result: SuccessfulCompareResult,
    ) -> Result<AutoScanComparePublication, String> {
        let owner = result.owner().clone();
        let _gate = self.gate.lock().unwrap();
        let active_guard = self.active.lock().unwrap();
        let Some(active) = active_guard.as_ref().filter(|active| {
            active.generation == permit.generation && active.binding.owns_compare(&owner)
        }) else {
            return Err("The AutoScan generation stopped before Compare completed".into());
        };
        let mut shared = active
            .shared
            .lock()
            .map_err(|_| "AutoScan status lock is poisoned".to_string())?;
        match &shared.ticket {
            AutoScanTicketLifecycle::CompareRunning {
                trigger,
                permit: expected,
            } if trigger.generation == active.generation
                && permit.owns_compare(&owner)
                && expected == permit => {}
            AutoScanTicketLifecycle::Idle
            | AutoScanTicketLifecycle::AutoApplyCompleted { .. }
            | AutoScanTicketLifecycle::AutoApplyClaimed { .. }
            | AutoScanTicketLifecycle::AutoApplyAuthorized { .. } => {
                return Err("The AutoScan ticket completed before Compare".into());
            }
            AutoScanTicketLifecycle::AwaitingCompare { .. }
            | AutoScanTicketLifecycle::ComparePermitted { .. }
            | AutoScanTicketLifecycle::CompareRunning { .. } => {
                return Err("This Compare does not own the pending AutoScan permit".into());
            }
        }
        let publication = self
            .execution
            .results
            .publish_successful_version(permit.verification(), result)
            .map_err(|error| error.to_string())?;
        shared.status.job_name = Some(owner.job_name.clone());
        shared.status.detail = "Verification complete; waiting for changes".into();
        shared.ticket = if active.binding.auto_apply {
            AutoScanTicketLifecycle::AutoApplyCompleted {
                ticket: AutoApplyTicket {
                    generation: permit.generation,
                    ticket_id: permit.ticket_id,
                    compare_identity: owner.identity,
                },
            }
        } else {
            AutoScanTicketLifecycle::Idle
        };
        let worker_stopped = active
            .commands
            .send(WorkerCommand::VerificationPublished {
                ticket_id: permit.ticket_id,
            })
            .is_err();
        let status = if worker_stopped {
            mark_shared_inactive(
                &mut shared,
                "AutoScan stopped safely because its worker disconnected after Compare publication",
            )
        } else {
            shared.snapshot()
        };
        let events = active.events.clone();
        drop(shared);
        drop(active_guard);
        drop(_gate);
        if worker_stopped {
            *self.tombstone.lock().unwrap() = Some(status.clone());
        }
        events.emit_status(status.clone());
        Ok(AutoScanComparePublication {
            publication,
            autoscan_status: status,
        })
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
        let ticket = match &shared.ticket {
            AutoScanTicketLifecycle::AutoApplyCompleted { ticket }
                if ticket.matches_key(generation, ticket_id)
                    && active_owns_ticket(active, ticket) =>
            {
                ticket.clone()
            }
            AutoScanTicketLifecycle::Idle
            | AutoScanTicketLifecycle::AwaitingCompare { .. }
            | AutoScanTicketLifecycle::ComparePermitted { .. }
            | AutoScanTicketLifecycle::CompareRunning { .. } => {
                return Err(
                    "This AutoScan ticket has no completed AutoApply result to claim".into(),
                );
            }
            AutoScanTicketLifecycle::AutoApplyCompleted { .. }
            | AutoScanTicketLifecycle::AutoApplyClaimed { .. }
            | AutoScanTicketLifecycle::AutoApplyAuthorized { .. } => {
                return Err("This AutoScan AutoApply ticket is stale or was already used".into());
            }
        };
        shared.ticket = AutoScanTicketLifecycle::AutoApplyClaimed {
            ticket: ticket.clone(),
        };
        Ok(ticket)
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
        let claimed = match &shared.ticket {
            AutoScanTicketLifecycle::AutoApplyClaimed { ticket: claimed }
                if claimed.same_authority(ticket) =>
            {
                claimed.clone()
            }
            AutoScanTicketLifecycle::Idle
            | AutoScanTicketLifecycle::AwaitingCompare { .. }
            | AutoScanTicketLifecycle::ComparePermitted { .. }
            | AutoScanTicketLifecycle::CompareRunning { .. } => {
                return Err("This AutoScan AutoApply claim is no longer active".into());
            }
            AutoScanTicketLifecycle::AutoApplyCompleted { .. }
            | AutoScanTicketLifecycle::AutoApplyClaimed { .. }
            | AutoScanTicketLifecycle::AutoApplyAuthorized { .. } => {
                return Err("This AutoScan AutoApply claim is stale or was already used".into());
            }
        };
        match issue() {
            Ok(value) => {
                shared.ticket = AutoScanTicketLifecycle::AutoApplyAuthorized { ticket: claimed };
                Ok(value)
            }
            Err(error) => {
                // The claim itself is one-use. A failed grant lookup or token mint cannot be retried
                // by retaining an internal ticket value after the public claim edge has closed.
                shared.ticket = AutoScanTicketLifecycle::Idle;
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
        match &shared.ticket {
            AutoScanTicketLifecycle::AutoApplyAuthorized { ticket: authorized }
                if authorized.same_authority(ticket) => {}
            AutoScanTicketLifecycle::Idle
            | AutoScanTicketLifecycle::AwaitingCompare { .. }
            | AutoScanTicketLifecycle::ComparePermitted { .. }
            | AutoScanTicketLifecycle::CompareRunning { .. } => {
                return Err("This AutoScan AutoApply authorization is no longer active".into());
            }
            AutoScanTicketLifecycle::AutoApplyCompleted { .. }
            | AutoScanTicketLifecycle::AutoApplyClaimed { .. }
            | AutoScanTicketLifecycle::AutoApplyAuthorized { .. } => {
                return Err(
                    "This AutoScan AutoApply authorization is stale or already used".into(),
                );
            }
        }
        shared.ticket = AutoScanTicketLifecycle::Idle;
        reserve()
    }
}

fn terminalize_repository_verification(
    execution: &AutoScanExecutionServices,
    binding: &AutoScanBinding,
    verification: &CompareVerificationTicket,
    terminal: &AutoScanVerificationTerminal,
) -> Option<CompareScopeExecutionStatusDto> {
    execution
        .results
        .complete_verification_terminal(verification, terminal.repository_outcome())
        .then(|| execution.results.execution_status(&binding.compare_scope()))
}

fn emit_execution_transition(
    app: &tauri::AppHandle,
    status: Option<CompareScopeExecutionStatusDto>,
) {
    if let Some(status) = status {
        let _ = app.emit_to(MAIN_WINDOW_LABEL, "compare-execution-status", status);
    }
}

fn active_owns_ticket(active: &ActiveAutoScan, ticket: &AutoApplyTicket) -> bool {
    active.generation == ticket.generation
        && active.binding.auto_apply
        && active.binding.job_id == ticket.compare_identity.job_id
        && active.binding.config_revision == ticket.compare_identity.config_revision
        && active.binding.target_index == ticket.compare_identity.target_index
}

impl Drop for AutoScanController {
    fn drop(&mut self) {
        if let Ok(active) = self.active.get_mut() {
            if let Some(active) = active.as_mut() {
                let (snapshot, execution_status) = {
                    let mut shared = active.shared.lock().unwrap();
                    let execution_status = shared.ticket.verification().and_then(|verification| {
                        terminalize_repository_verification(
                            &self.execution,
                            &active.binding,
                            verification,
                            &AutoScanVerificationTerminal::Cancelled,
                        )
                    });
                    (
                        mark_shared_inactive(&mut shared, "AutoScan stopped during shutdown"),
                        execution_status,
                    )
                };
                active.events.emit_execution_transition(execution_status);
                active.events.emit_status(snapshot);
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
    shared.status.detail = detail.into();
    shared.ticket = AutoScanTicketLifecycle::Idle;
    shared.snapshot()
}

fn publish_status(
    app: &tauri::AppHandle,
    shared: &Arc<Mutex<AutoScanShared>>,
    mode: AutoScanDetectionMode,
    detail: impl Into<String>,
) {
    let snapshot = {
        let mut shared = shared.lock().unwrap();
        shared.status.mode = Some(mode.into());
        shared.status.detail = detail.into();
        shared.snapshot()
    };
    let _ = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", snapshot);
}

fn stop_for_ticket_cursor_exhaustion(app: &tauri::AppHandle, shared: &Arc<Mutex<AutoScanShared>>) {
    let snapshot = {
        let mut shared = shared.lock().unwrap();
        mark_shared_inactive(
            &mut shared,
            "AutoScan stopped safely because its ticket cursor was exhausted",
        )
    };
    let _ = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", snapshot);
}

fn next_ticket_id(ticket_id: u64) -> Option<u64> {
    ticket_id.checked_add(1)
}

fn begin_observed_trigger(
    binding: &AutoScanBinding,
    execution: &AutoScanExecutionServices,
) -> Result<CompareVerificationTicket, String> {
    let scope = binding.compare_scope();
    let verification = execution.results.begin_verification(scope.clone(), None);
    execution.authorizations.revoke_apply_authority(&scope);
    verification.map_err(|error| error.to_string())
}

#[derive(Clone, Copy)]
struct TriggerObservation {
    generation: u64,
    ticket_id: u64,
    mode: AutoScanDetectionMode,
    reason: AutoScanTriggerReason,
}

fn publish_trigger(
    app: &tauri::AppHandle,
    shared: &Arc<Mutex<AutoScanShared>>,
    execution: &AutoScanExecutionServices,
    binding: &AutoScanBinding,
    observation: TriggerObservation,
) -> bool {
    let prior_verification = {
        let shared = shared.lock().unwrap();
        if !shared.status.active || shared.status.generation != observation.generation {
            return false;
        }
        shared.ticket.verification().cloned()
    };
    if let Some(prior_verification) = prior_verification {
        let terminal = AutoScanVerificationTerminal::Failed(
            "AutoScan attempted to overlap an unfinished verification ticket".into(),
        );
        let execution_status =
            terminalize_repository_verification(execution, binding, &prior_verification, &terminal);
        emit_execution_transition(app, execution_status);
        let snapshot = {
            let mut shared = shared.lock().unwrap();
            mark_shared_inactive(&mut shared, terminal.status_detail())
        };
        let _ = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", snapshot);
        return false;
    }
    // Dirty the executable evidence before any trigger or stopped-status event is observable.
    // `with_fresh_execution_eligibility` uses the same repository lock, so final Apply reservation
    // either precedes this observation or fails; an Apply review issued first is revoked below.
    let verification = match begin_observed_trigger(binding, execution) {
        Ok(verification) => verification,
        Err(error) => {
            let _ = app.emit_to(
                MAIN_WINDOW_LABEL,
                "compare-execution-status",
                execution.results.execution_status(&binding.compare_scope()),
            );
            let snapshot = {
                let mut shared = shared.lock().unwrap();
                mark_shared_inactive(&mut shared, format!("AutoScan stopped safely: {error}"))
            };
            let _ = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", snapshot);
            return false;
        }
    };
    let _ = app.emit_to(
        MAIN_WINDOW_LABEL,
        "compare-execution-status",
        execution.results.execution_status(&binding.compare_scope()),
    );
    let job_name = match resolve_binding_job_name(binding) {
        Ok(job_name) => job_name,
        Err(error) => {
            let execution_status = terminalize_repository_verification(
                execution,
                binding,
                &verification,
                &AutoScanVerificationTerminal::Failed(error.clone()),
            );
            emit_execution_transition(app, execution_status);
            let snapshot = {
                let mut shared = shared.lock().unwrap();
                mark_shared_inactive(&mut shared, format!("AutoScan stopped safely: {error}"))
            };
            let _ = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", snapshot);
            return false;
        }
    };
    let trigger = AutoScanTriggerDto {
        generation: observation.generation,
        ticket_id: observation.ticket_id,
        job_id: binding.job_id.clone(),
        job_name,
        config_revision: binding.config_revision.clone(),
        target_index: binding.target_index,
        auto_apply: binding.auto_apply,
        mode: observation.mode,
        reason: observation.reason,
    };
    let accepted = {
        let mut shared = shared.lock().unwrap();
        if !shared.status.active || shared.status.generation != observation.generation {
            false
        } else {
            // New work supersedes any completed, claimed, or authorized predecessor before the event
            // becomes observable. A missed event is recoverable from this exact status snapshot.
            shared.status.latest_ticket_id = observation.ticket_id;
            shared.status.job_name = Some(trigger.job_name.clone());
            shared.status.mode = Some(observation.mode.into());
            shared.status.detail = match observation.reason {
                AutoScanTriggerReason::Bootstrap => "Running the initial verification".into(),
                AutoScanTriggerReason::FilesystemChange => {
                    "A filesystem change requested verification".into()
                }
                AutoScanTriggerReason::WatchInvalidated => {
                    "The native event history changed; a full verification is required".into()
                }
                AutoScanTriggerReason::PeriodicVerification => {
                    "Running the periodic full verification".into()
                }
            };
            shared.ticket = AutoScanTicketLifecycle::AwaitingCompare {
                trigger: trigger.clone(),
                verification: verification.clone(),
            };
            true
        }
    };
    if !accepted {
        let execution_status = terminalize_repository_verification(
            execution,
            binding,
            &verification,
            &AutoScanVerificationTerminal::Cancelled,
        );
        emit_execution_transition(app, execution_status);
        return false;
    }
    let _ = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-trigger", trigger);
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
    if binding.target_index >= job.targets.len() {
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
    execution: AutoScanExecutionServices,
) {
    #[cfg(target_os = "macos")]
    if let Some((source, target)) = local_roots {
        match run_native_macos(
            app,
            generation,
            &binding,
            (&source, &target),
            &commands,
            &shared,
            &execution,
        ) {
            NativeExit::Stopped => return,
            NativeExit::PollingRequired {
                detail,
                next_ticket,
            } => {
                run_polling(
                    app,
                    generation,
                    &binding,
                    &commands,
                    &shared,
                    &execution,
                    PollingStart {
                        detail,
                        next_ticket,
                        immediate: false,
                    },
                );
                return;
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    let polling_detail = if local_roots.is_some() {
        "Native filesystem events are not available on this platform; polling while SyncDash is open"
    } else {
        "These roots do not expose a local event journal; polling while SyncDash is open"
    };
    #[cfg(target_os = "macos")]
    let polling_detail =
        "These roots do not expose a local FSEvents journal; polling while SyncDash is open";
    run_polling(
        app,
        generation,
        &binding,
        &commands,
        &shared,
        &execution,
        PollingStart {
            detail: polling_detail.into(),
            next_ticket: 1,
            immediate: true,
        },
    );
}

struct PollingStart {
    detail: String,
    next_ticket: u64,
    immediate: bool,
}

fn run_polling(
    app: &tauri::AppHandle,
    generation: u64,
    binding: &AutoScanBinding,
    commands: &mpsc::Receiver<WorkerCommand>,
    shared: &Arc<Mutex<AutoScanShared>>,
    execution: &AutoScanExecutionServices,
    start: PollingStart,
) {
    publish_status(app, shared, AutoScanDetectionMode::Polling, start.detail);
    let interval = binding.interval();
    let mut deadline = if start.immediate {
        Instant::now()
    } else {
        Instant::now() + interval
    };
    let mut next_ticket = start.next_ticket;
    let mut awaiting = shared
        .lock()
        .unwrap()
        .ticket
        .pending_trigger()
        .map(|trigger| trigger.ticket_id);
    loop {
        match commands.recv_timeout(WORKER_TICK) {
            Ok(WorkerCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(
                WorkerCommand::VerificationPublished { ticket_id }
                | WorkerCommand::VerificationTerminated { ticket_id },
            ) => {
                if awaiting == Some(ticket_id) {
                    awaiting = None;
                    deadline = Instant::now() + interval;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if awaiting.is_none() && Instant::now() >= deadline {
            let Some(successor_ticket) = next_ticket_id(next_ticket) else {
                stop_for_ticket_cursor_exhaustion(app, shared);
                return;
            };
            let reason = if next_ticket == 1 {
                AutoScanTriggerReason::Bootstrap
            } else {
                AutoScanTriggerReason::PeriodicVerification
            };
            if !publish_trigger(
                app,
                shared,
                execution,
                binding,
                TriggerObservation {
                    generation,
                    ticket_id: next_ticket,
                    mode: AutoScanDetectionMode::Polling,
                    reason,
                },
            ) {
                return;
            }
            awaiting = Some(next_ticket);
            next_ticket = successor_ticket;
        }
    }
}

#[cfg(target_os = "macos")]
enum NativeExit {
    Stopped,
    PollingRequired { detail: String, next_ticket: u64 },
}

#[cfg(target_os = "macos")]
fn run_native_macos(
    app: &tauri::AppHandle,
    generation: u64,
    binding: &AutoScanBinding,
    roots: (&std::path::Path, &std::path::Path),
    commands: &mpsc::Receiver<WorkerCommand>,
    shared: &Arc<Mutex<AutoScanShared>>,
    execution: &AutoScanExecutionServices,
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
    let watcher = match syncdash::fs::watch::macos::watch_pair(roots.0, roots.1, resume.as_ref()) {
        Ok(watcher) => watcher,
        Err(error) => {
            return NativeExit::PollingRequired {
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
        return NativeExit::PollingRequired {
            detail: format!(
                "FSEvents returned an invalid cursor ({error}); polling while SyncDash is open"
            ),
            next_ticket: 1,
        };
    }
    publish_status(
        app,
        shared,
        AutoScanDetectionMode::NativeFsevents,
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
                Ok(WorkerCommand::VerificationPublished { ticket_id }) => {
                    let completed =
                        trigger.complete_success(ticket_id, |position| checkpoint.save(position));
                    if let Err(error) = completed {
                        syncdash::log_warn!("autoscan", "AutoScan work was not committed: {error}");
                    }
                    periodic_deadline = Instant::now() + interval;
                    retry_not_before = periodic_deadline;
                }
                Ok(WorkerCommand::VerificationTerminated { ticket_id }) => {
                    let completed = trigger
                        .complete_failure(ticket_id)
                        .map_err(std::io::Error::other);
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
                    return NativeExit::PollingRequired {
                        detail: format!(
                            "FSEvents cursor continuity failed ({error}); polling while SyncDash is open"
                        ),
                        next_ticket,
                    };
                }
            }
            Ok(WatchMessage::BackendError { message, .. }) => {
                return NativeExit::PollingRequired {
                    detail: format!(
                        "FSEvents stopped reporting reliably ({message}); polling while SyncDash is open"
                    ),
                    next_ticket,
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return NativeExit::PollingRequired {
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
                        return NativeExit::PollingRequired {
                            detail: format!(
                                "FSEvents periodic cursor capture failed ({error}); polling while SyncDash is open"
                            ),
                            next_ticket,
                        };
                    }
                }
                Err(error) => {
                    return NativeExit::PollingRequired {
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
                return NativeExit::PollingRequired {
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
            } => AutoScanTriggerReason::Bootstrap,
            WorkCoverage::FullTree {
                reason: FullScanReason::Periodic,
            } => AutoScanTriggerReason::PeriodicVerification,
            WorkCoverage::FullTree {
                reason: FullScanReason::WatchInvalidated(_),
            }
            | WorkCoverage::FullTree {
                reason: FullScanReason::ChangeSetTooLarge { .. },
            } => AutoScanTriggerReason::WatchInvalidated,
            WorkCoverage::FullTree { .. } | WorkCoverage::IncrementalEligible { .. } => {
                AutoScanTriggerReason::FilesystemChange
            }
        };
        let Some(successor_ticket) = next_ticket_id(ticket.id) else {
            stop_for_ticket_cursor_exhaustion(app, shared);
            return NativeExit::Stopped;
        };
        next_ticket = next_ticket.max(successor_ticket);
        if !publish_trigger(
            app,
            shared,
            execution,
            binding,
            TriggerObservation {
                generation,
                ticket_id: ticket.id,
                mode: AutoScanDetectionMode::NativeFsevents,
                reason,
            },
        ) {
            return NativeExit::Stopped;
        }
    }
}

pub(crate) fn configured_interval(job: &syncdash::job::Job) -> u64 {
    job.autoscan_interval_secs
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
                result_id: "11111111111111111111111111111111".into(),
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
        job.autoscan_interval_secs = Some(0);
        assert_eq!(configured_interval(&job), 1);
        job.autoscan_interval_secs = Some(90);
        assert_eq!(configured_interval(&job), 90);
    }

    #[test]
    fn counters_fail_closed_without_consuming_pending_work() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(
            allocate_unique_id(&counter, "test identity").unwrap(),
            u64::MAX
        );
        assert!(allocate_unique_id(&counter, "test identity")
            .unwrap_err()
            .contains("ID space is exhausted"));
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
        assert_eq!(next_ticket_id(u64::MAX - 1), Some(u64::MAX));
        assert_eq!(next_ticket_id(u64::MAX), None);

        let (controller, _receiver) = controller_waiting_for(29, false);
        controller
            .compare_permits
            .store(u64::MAX, Ordering::Relaxed);
        assert!(controller.issue_compare_permit(4, 29).is_err());
        assert_eq!(controller.status().active_ticket, Some(29));
        assert!(controller.decline_trigger(4, 29).is_ok());
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
            mode: AutoScanDetectionMode::Polling,
            reason: AutoScanTriggerReason::FilesystemChange,
        }
    }

    fn controller() -> AutoScanController {
        AutoScanController::new(
            Arc::new(CompareResultRepository::default()),
            Arc::new(OperationAuthorizationStore::default()),
        )
    }

    fn controller_waiting_for(
        ticket_id: u64,
        auto_apply: bool,
    ) -> (AutoScanController, mpsc::Receiver<WorkerCommand>) {
        let controller = controller();
        let mut binding = binding();
        binding.auto_apply = auto_apply;
        let (commands, receiver) = mpsc::channel();
        let mut status = AutoScanStatusCore::starting(4, &binding);
        status.latest_ticket_id = ticket_id;
        let pending_trigger = trigger(&binding, 4, ticket_id);
        let verification = controller
            .execution
            .results
            .begin_verification(binding.compare_scope(), None)
            .unwrap();
        *controller.active.lock().unwrap() = Some(ActiveAutoScan {
            binding,
            generation: 4,
            commands,
            shared: Arc::new(Mutex::new(AutoScanShared {
                status,
                ticket: AutoScanTicketLifecycle::AwaitingCompare {
                    trigger: pending_trigger,
                    verification,
                },
            })),
            events: AutoScanEvents::Suppressed,
            join: None,
        });
        (controller, receiver)
    }

    fn owner() -> CompareOwner {
        CompareOwner {
            identity: crate::dto::CompareIdentity {
                result_id: "22222222222222222222222222222222".into(),
                compare_run_id: 8,
                job_id: "job-id-photos".into(),
                config_revision: "revision-a".into(),
                target_index: 1,
            },
            job_name: "photos".into(),
        }
    }

    fn successful_result(owner: CompareOwner) -> SuccessfulCompareResult {
        let plan = crate::dto::PlanDto {
            owner,
            header: syncdash::model::plan::PlanHeader {
                schema: syncdash::model::plan::PLAN_SCHEMA,
                kind: "plan".into(),
                mode: "mirror".into(),
                generated_at_ms: 0,
                source_root: "/source".into(),
                source_host: "host".into(),
                target_root: "/target".into(),
                target_host: "host".into(),
                op_count: 0,
                conflict_count: 0,
                source_entries: 0,
                target_entries: 0,
                source_excluded: 0,
                target_excluded: 0,
                source_walk_errors: 0,
                target_walk_errors: 0,
                source_walk_err_samples: Vec::new(),
                target_walk_err_samples: Vec::new(),
                source_icloud_stubs: 0,
                target_icloud_stubs: 0,
                source_icloud_stub_samples: Vec::new(),
                target_icloud_stub_samples: Vec::new(),
            },
            ops: Vec::new(),
            metas: Vec::new(),
            identical_count: 0,
            identical_bytes: 0,
        };
        let snapshot = |root: &str| syncdash::model::table::TableArtifact {
            header: syncdash::model::table::TableHeader {
                schema: syncdash::model::table::TABLE_SCHEMA,
                kind: syncdash::model::table::TableKind::Snapshot,
                root: root.into(),
                host: "host".into(),
                os: "test".into(),
                scanned_at_ms: 0,
                duration_ms: 0,
                entry_count: 0,
                evidence: syncdash::model::table::TableEvidence::None,
                excluded_dirs: 0,
                excluded_files: 0,
                walk_errors: 0,
                walk_err_samples: Vec::new(),
                icloud_stubs: 0,
                icloud_stub_samples: Vec::new(),
                skipped_symlinks: 0,
                dataless_files: 0,
                vfs: None,
            },
            entries: Vec::new(),
        };
        SuccessfulCompareResult::from_plan(
            "test-plan-digest".into(),
            plan,
            snapshot("/source"),
            snapshot("/target"),
            syncdash::pipeline::compare::CompareOptions::default(),
        )
    }

    fn publish_current_compare(controller: &AutoScanController) -> AutoScanComparePublication {
        let status = controller.status();
        let ticket_id = status.active_ticket.unwrap();
        let permit = controller.issue_compare_permit(4, ticket_id).unwrap();
        let owner = owner();
        controller
            .mark_compare_launched(&permit, owner.identity.compare_run_id)
            .unwrap();
        controller
            .publish_successful_compare(&permit, successful_result(owner))
            .unwrap()
    }

    fn pending_verification(controller: &AutoScanController) -> CompareVerificationTicket {
        let shared = controller
            .active
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .shared
            .clone();
        let verification = match &shared.lock().unwrap().ticket {
            AutoScanTicketLifecycle::AwaitingCompare { verification, .. } => verification.clone(),
            _ => panic!("test controller must be awaiting Compare"),
        };
        verification
    }

    #[test]
    fn decline_is_one_use_and_requires_the_exact_unlaunched_trigger() {
        let (controller, receiver) = controller_waiting_for(11, false);
        assert!(controller.decline_trigger(3, 11).is_err());
        assert!(controller.decline_trigger(4, 12).is_err());
        controller.issue_compare_permit(4, 11).unwrap();
        let status = controller.decline_trigger(4, 11).unwrap();
        assert_eq!(status.active_ticket, None);
        assert!(matches!(
            controller
                .execution
                .results
                .execution_status(&binding().compare_scope()),
            CompareScopeExecutionStatusDto::Failed { message, .. }
                if message == "AutoScan verification failed: The trigger was declined before Compare launched"
        ));
        assert!(controller.decline_trigger(4, 11).is_err());
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::VerificationTerminated { ticket_id: 11 })
        ));
    }

    #[test]
    fn observed_trigger_advances_verification_before_ticket_state_is_visible() {
        let (controller, _receiver) = controller_waiting_for(11, false);
        let previous = pending_verification(&controller);
        let binding = controller
            .active
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .binding
            .clone();

        let next = begin_observed_trigger(&binding, &controller.execution).unwrap();

        assert_ne!(next, previous);
        assert_eq!(pending_verification(&controller), previous);
    }

    #[test]
    fn successful_publication_requires_the_authenticated_running_compare() {
        let (controller, receiver) = controller_waiting_for(12, true);
        let forged_manual_permit = AutoScanComparePermit {
            permit_id: 999,
            generation: 4,
            ticket_id: 12,
            job_id: "job-id-photos".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
            verification: pending_verification(&controller),
        };
        assert!(controller
            .publish_successful_compare(&forged_manual_permit, successful_result(owner()),)
            .is_err());

        let permit = controller.issue_compare_permit(4, 12).unwrap();
        let mut wrong = owner();
        wrong.identity.target_index = 0;
        assert!(controller
            .publish_successful_compare(&permit, successful_result(wrong))
            .is_err());
        assert!(receiver.try_recv().is_err());
        controller
            .mark_compare_launched(&permit, owner().identity.compare_run_id)
            .unwrap();
        assert!(controller.mark_compare_launched(&permit, 99).is_err());
        let mut expected_owner = owner();
        expected_owner.job_name = "externally-renamed".into();
        let completed = controller
            .publish_successful_compare(&permit, successful_result(expected_owner))
            .unwrap();
        assert_eq!(completed.autoscan_status.active_ticket, None);
        assert_eq!(completed.autoscan_status.pending_trigger, None);
        assert_eq!(
            completed.autoscan_status.job_name.as_deref(),
            Some("externally-renamed")
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::VerificationPublished { ticket_id: 12 })
        ));
        let ticket = controller.claim_completed_auto_apply(4, 12).unwrap();
        assert_eq!(ticket.compare_identity(), &owner().identity);
    }

    #[test]
    fn published_evidence_survives_worker_disconnect_but_autoapply_stops_fail_closed() {
        let (controller, receiver) = controller_waiting_for(32, true);
        drop(receiver);
        let permit = controller.issue_compare_permit(4, 32).unwrap();
        let expected_owner = owner();
        controller
            .mark_compare_launched(&permit, expected_owner.identity.compare_run_id)
            .unwrap();

        let completed = controller
            .publish_successful_compare(&permit, successful_result(expected_owner.clone()))
            .unwrap();

        assert!(!completed.autoscan_status.active);
        assert_eq!(completed.autoscan_status.active_ticket, None);
        assert!(controller
            .execution
            .results
            .get_exact(&expected_owner.identity)
            .unwrap()
            .is_some());
        assert!(controller.claim_completed_auto_apply(4, 32).is_err());
    }

    #[test]
    fn abandoned_permitted_compare_terminalizes_and_releases_its_ticket() {
        let (controller, receiver) = controller_waiting_for(27, false);
        let permit = controller.issue_compare_permit(4, 27).unwrap();
        let status = controller
            .terminalize_permitted_verification(
                &permit,
                AutoScanVerificationTerminal::Failed("review was abandoned".into()),
            )
            .unwrap();
        assert_eq!(status.active_ticket, None);
        assert_eq!(status.pending_trigger, None);
        assert!(matches!(
            controller
                .execution
                .results
                .execution_status(&binding().compare_scope()),
            CompareScopeExecutionStatusDto::Failed { message, .. }
                if message == "AutoScan verification failed: review was abandoned"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::VerificationTerminated { ticket_id: 27 })
        ));
        assert!(controller.issue_compare_permit(4, 27).is_err());
    }

    #[test]
    fn ui_decline_cannot_terminalize_a_compare_after_backend_launch() {
        let (controller, receiver) = controller_waiting_for(31, false);
        let permit = controller.issue_compare_permit(4, 31).unwrap();
        controller
            .mark_compare_launched(&permit, owner().identity.compare_run_id)
            .unwrap();

        assert!(controller.decline_trigger(4, 31).is_err());
        assert_eq!(controller.status().active_ticket, Some(31));
        assert!(receiver.try_recv().is_err());

        controller
            .terminalize_permitted_verification(&permit, AutoScanVerificationTerminal::Cancelled)
            .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::VerificationTerminated { ticket_id: 31 })
        ));
    }

    #[test]
    fn stopped_generation_rejects_publication_before_repository_transition() {
        let (controller, _receiver) = controller_waiting_for(28, false);
        let permit = controller.issue_compare_permit(4, 28).unwrap();
        controller
            .mark_compare_launched(&permit, owner().identity.compare_run_id)
            .unwrap();
        controller.stop();
        let expected_owner = owner();
        assert!(controller
            .publish_successful_compare(&permit, successful_result(expected_owner.clone()),)
            .is_err());
        assert!(controller
            .execution
            .results
            .get_exact(&expected_owner.identity)
            .unwrap()
            .is_none());
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
        let status = publish_current_compare(&controller).autoscan_status;
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
        publish_current_compare(&controller);
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
        publish_current_compare(&controller);
        let ticket = controller.claim_completed_auto_apply(4, 21).unwrap();
        assert!(controller
            .authorize_claim(&ticket, || Err::<(), _>("grant disappeared".into()))
            .is_err());
        assert!(controller.claim_completed_auto_apply(4, 21).is_err());
        assert!(controller
            .authorize_claim(&ticket, || Ok::<_, String>(()))
            .is_err());

        let (controller, _receiver) = controller_waiting_for(22, true);
        publish_current_compare(&controller);
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
    fn decline_or_non_autoapply_publication_never_creates_write_authority() {
        let (failed, _receiver) = controller_waiting_for(16, true);
        failed.decline_trigger(4, 16).unwrap();
        assert!(failed.claim_completed_auto_apply(4, 16).is_err());

        let (manual, _receiver) = controller_waiting_for(17, false);
        publish_current_compare(&manual);
        assert!(manual.claim_completed_auto_apply(4, 17).is_err());
    }

    #[test]
    fn stop_discards_a_claim_and_rename_relabels_without_invalidating_it() {
        let (controller, _receiver) = controller_waiting_for(18, true);
        publish_current_compare(&controller);
        controller
            .rebind_job_name("job-id-photos", "renamed-after-publication")
            .unwrap();
        let ticket = controller.claim_completed_auto_apply(4, 18).unwrap();
        assert_eq!(ticket.compare_identity(), &owner().identity);
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
        let never_started = controller().status();
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
        assert!(matches!(
            controller
                .execution
                .results
                .execution_status(&binding().compare_scope()),
            CompareScopeExecutionStatusDto::Cancelled { .. }
        ));
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
    fn concurrent_duplicate_declines_have_exactly_one_winner() {
        let (controller, receiver) = controller_waiting_for(19, false);
        let controller = Arc::new(controller);
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let controller = controller.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                controller.decline_trigger(4, 19).is_ok()
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
            Ok(WorkerCommand::VerificationTerminated { ticket_id: 19 })
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
