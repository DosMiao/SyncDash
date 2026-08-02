//! Worker generation start, stop, rebinding, and status queries.

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use super::transition::terminalize_repository_verification;
use super::AutoScanController;
use crate::features::autoscan::model::{
    AutoScanBinding, AutoScanStatusDto, AutoScanVerificationTerminal,
};
use crate::features::autoscan::runtime::{
    allocate_unique_id, ActiveAutoScan, AutoScanEvents, WorkerCommand,
};
use crate::features::autoscan::state::{
    AutoScanShared, AutoScanStatusCore, AutoScanTicketLifecycle,
};
use crate::features::autoscan::worker::observation::mark_shared_inactive;
use crate::features::autoscan::worker::run_worker;

impl AutoScanController {
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
}
