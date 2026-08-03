//! Fail-closed controller cleanup when the desktop process shuts down.

use crate::features::autoscan::runtime::WorkerCommand;

use super::transition::deactivate_generation;
use super::AutoScanController;

impl Drop for AutoScanController {
    fn drop(&mut self) {
        // Shutdown must not panic on a poisoned generation lock: the process is already going
        // away, and a panic here would take the rest of the teardown with it.
        if let Ok(active) = self.active.get_mut() {
            if let Some(active) = active.as_ref() {
                let (snapshot, execution_status) = deactivate_generation(
                    &self.execution,
                    active,
                    "AutoScan stopped during shutdown",
                );
                active.events.emit_execution_transition(execution_status);
                active.events.emit_status(snapshot);
                let _ = active.commands.send(WorkerCommand::Stop);
            }
        }
    }
}
