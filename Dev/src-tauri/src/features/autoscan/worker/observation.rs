//! Publication of status and trigger observations after evidence is dirtied.

use std::sync::{Arc, Mutex};

use tauri::Emitter;

use crate::features::autoscan::controller::transition::{
    emit_execution_transition, terminalize_repository_verification,
};
use crate::features::compare::evidence::model::verification::CompareVerificationTicket;
use crate::window::MAIN_WINDOW_LABEL;

use super::super::model::{
    AutoScanBinding, AutoScanDetectionMode, AutoScanStatusDto, AutoScanTriggerDto,
    AutoScanTriggerReason, AutoScanVerificationTerminal,
};
use super::super::runtime::AutoScanExecutionServices;
use super::super::state::{AutoScanShared, AutoScanTicketLifecycle};
use super::binding::resolve_binding_job_name;

pub(in crate::features::autoscan) fn mark_shared_inactive(
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

pub(super) fn publish_status(
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

pub(super) fn stop_for_ticket_cursor_exhaustion(
    app: &tauri::AppHandle,
    shared: &Arc<Mutex<AutoScanShared>>,
) {
    let snapshot = {
        let mut shared = shared.lock().unwrap();
        mark_shared_inactive(
            &mut shared,
            "AutoScan stopped safely because its ticket cursor was exhausted",
        )
    };
    let _ = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", snapshot);
}

pub(in crate::features::autoscan) fn begin_observed_trigger(
    binding: &AutoScanBinding,
    execution: &AutoScanExecutionServices,
) -> Result<CompareVerificationTicket, String> {
    let scope = binding.compare_scope();
    let verification = execution.results.begin_verification(scope.clone(), None);
    execution.authorizations.revoke_apply_authority(&scope);
    verification.map_err(|error| error.to_string())
}

#[derive(Clone, Copy)]
pub(super) struct TriggerObservation {
    pub(super) generation: u64,
    pub(super) ticket_id: u64,
    pub(super) mode: AutoScanDetectionMode,
    pub(super) reason: AutoScanTriggerReason,
}

pub(super) fn publish_trigger(
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
