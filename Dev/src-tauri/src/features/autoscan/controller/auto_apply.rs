//! One-use AutoApply claim, authorization, and final consumption transitions.

use super::ownership::active_owns_ticket;
use super::AutoScanController;
use crate::features::autoscan::state::AutoScanTicketLifecycle;
use crate::features::operations::autoscan_authority::AutoApplyTicket;

impl AutoScanController {
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
