//! FSEvents detection, checkpoint continuity, and polling fallback policy.

use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use syncdash::fs::watch::WatchMessage;
use syncdash::run::watch::{
    ChangeBatch, FullScanReason, RigorPolicy, WatchConfig, WatchTrigger, WorkCoverage,
};
use syncdash::store::watch::{CheckpointLoad, CheckpointStore};

use super::super::model::{AutoScanBinding, AutoScanDetectionMode, AutoScanTriggerReason};
use super::super::runtime::{AutoScanExecutionServices, WorkerCommand};
use super::super::state::AutoScanShared;
use super::configuration::{next_ticket_id, WORKER_TICK};
use super::observation::{
    publish_status, publish_trigger, stop_for_ticket_cursor_exhaustion, TriggerObservation,
};

pub(super) enum NativeExit {
    Stopped,
    PollingRequired { detail: String, next_ticket: u64 },
}

pub(super) fn run_native_macos(
    app: &tauri::AppHandle,
    generation: u64,
    binding: &AutoScanBinding,
    roots: (&std::path::Path, &std::path::Path),
    commands: &mpsc::Receiver<WorkerCommand>,
    shared: &Arc<Mutex<AutoScanShared>>,
    execution: &AutoScanExecutionServices,
) -> NativeExit {
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
