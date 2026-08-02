//! RAII leases that release command, run, and registry-mutation ownership on every exit path.

use std::sync::Arc;

use syncdash::obs::progress::RunCtl;

use super::coordinator::RunLifecycle;
use super::model::RunPurpose;

pub(crate) struct RunCommandLease {
    pub(super) lifecycle: Arc<RunLifecycle>,
}

impl RunCommandLease {
    pub(crate) fn start_run(&self, purpose: RunPurpose) -> Result<ActiveRunLease, String> {
        self.lifecycle.start_run(purpose, None)
    }

    pub(crate) fn start_apply_from_progress_launch(
        &self,
        progress_launch_id: u64,
    ) -> Result<ActiveRunLease, String> {
        self.lifecycle
            .start_run(RunPurpose::Apply, Some(progress_launch_id))
    }
}

impl Drop for RunCommandLease {
    fn drop(&mut self) {
        let mut state = self.lifecycle.state.lock().unwrap();
        state.commands_in_flight = state
            .commands_in_flight
            .checked_sub(1)
            .expect("run command lease count must not underflow");
    }
}

pub(crate) struct ActiveRunLease {
    pub(super) lifecycle: Arc<RunLifecycle>,
    pub(super) run_id: u64,
    pub(super) control: Arc<RunCtl>,
}

impl ActiveRunLease {
    pub(crate) fn run_id(&self) -> u64 {
        self.run_id
    }

    pub(crate) fn control(&self) -> Arc<RunCtl> {
        self.control.clone()
    }
}

impl Drop for ActiveRunLease {
    fn drop(&mut self) {
        let mut state = self.lifecycle.state.lock().unwrap();
        if state.active_run.as_ref().map(|run| run.run_id) == Some(self.run_id) {
            state.active_run = None;
        }
    }
}

pub(super) struct RegistryMutationLease<'a> {
    pub(super) lifecycle: &'a RunLifecycle,
}

impl Drop for RegistryMutationLease<'_> {
    fn drop(&mut self) {
        self.lifecycle
            .state
            .lock()
            .unwrap()
            .registry_mutation_in_progress = false;
    }
}
