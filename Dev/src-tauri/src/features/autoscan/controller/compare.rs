//! Compare permit issuance, launch authentication, and evidence publication.

use super::AutoScanController;
use crate::features::autoscan::authority::{AutoApplyTicket, AutoScanComparePermit};
use crate::features::autoscan::model::AutoScanTriggerDto;
use crate::features::autoscan::runtime::{
    allocate_unique_id, AutoScanComparePublication, WorkerCommand,
};
use crate::features::autoscan::state::AutoScanTicketLifecycle;
use crate::features::autoscan::worker::observation::mark_shared_inactive;
use crate::features::compare::evidence::model::result::SuccessfulCompareResult;

impl AutoScanController {
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
        let permit = AutoScanComparePermit::new(
            allocate_unique_id(&self.compare_permits, "AutoScan Compare permit")?,
            pending.generation,
            pending.ticket_id,
            pending.job_id.clone(),
            pending.config_revision.clone(),
            pending.target_index,
            verification,
        );
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
            .filter(|active| active.generation == permit.generation())
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
            active.generation == permit.generation() && active.binding.owns_compare(&owner)
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
                ticket: AutoApplyTicket::new(
                    permit.generation(),
                    permit.ticket_id(),
                    owner.identity,
                ),
            }
        } else {
            AutoScanTicketLifecycle::Idle
        };
        let worker_stopped = active
            .commands
            .send(WorkerCommand::VerificationPublished {
                ticket_id: permit.ticket_id(),
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
}
