//! Fail-closed controller cleanup when the desktop process shuts down.

use crate::features::autoscan::model::AutoScanVerificationTerminal;
use crate::features::autoscan::runtime::WorkerCommand;
use crate::features::autoscan::worker::observation::mark_shared_inactive;

use super::transition::terminalize_repository_verification;
use super::AutoScanController;

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
