//! Terminal transitions for pending and running verification tickets.

use super::transition::terminalize_repository_verification;
use super::AutoScanController;
use crate::features::autoscan::model::{AutoScanStatusDto, AutoScanVerificationTerminal};
use crate::features::autoscan::runtime::{AutoScanTerminalAuthority, WorkerCommand};
use crate::features::autoscan::state::AutoScanTicketLifecycle;
use crate::features::autoscan::worker::observation::mark_shared_inactive;
use crate::features::operations::autoscan_authority::AutoScanComparePermit;

impl AutoScanController {
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
            permit.generation(),
            permit.ticket_id(),
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
}
