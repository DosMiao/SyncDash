//! Polling detection with one outstanding verification ticket at a time.

use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use super::super::model::{AutoScanBinding, AutoScanDetectionMode, AutoScanTriggerReason};
use super::super::runtime::{AutoScanExecutionServices, WorkerCommand};
use super::super::state::AutoScanShared;
use super::configuration::{next_ticket_id, WORKER_TICK};
use super::observation::{
    publish_status, publish_trigger, stop_for_ticket_cursor_exhaustion, TriggerObservation,
};

pub(super) struct PollingStart {
    pub(super) detail: String,
    pub(super) next_ticket: u64,
    pub(super) immediate: bool,
}

pub(super) fn run_polling(
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
