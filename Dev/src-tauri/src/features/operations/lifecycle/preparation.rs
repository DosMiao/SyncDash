//! Command admission and active-run creation.

use std::sync::Arc;

use syncdash::obs::progress::RunCtl;

use super::lease::{ActiveRunLease, RunCommandLease};
use super::model::RunPurpose;
use super::reservation::pending_progress_launch_mut;
use super::state::{
    ActiveRun, ProgressLaunchPhase, ANOTHER_RUN_IS_ACTIVE, LAUNCH_IS_ALREADY_PREPARING,
};
use super::RunLifecycle;

impl RunLifecycle {
    pub(crate) fn command_lease(self: &Arc<Self>) -> Result<RunCommandLease, String> {
        let mut state = self.state.lock().unwrap();
        if state.registry_mutation_in_progress {
            return Err(
                "A job or settings update is in progress — wait a moment and try again".into(),
            );
        }
        state.commands_in_flight = state
            .commands_in_flight
            .checked_add(1)
            .ok_or_else(|| "The run command counter is exhausted — restart SyncDash".to_string())?;
        Ok(RunCommandLease {
            lifecycle: self.clone(),
        })
    }

    pub(super) fn start_run(
        self: &Arc<Self>,
        purpose: RunPurpose,
        progress_launch_id: Option<u64>,
    ) -> Result<ActiveRunLease, String> {
        let mut state = self.state.lock().unwrap();
        if state.active_run.is_some() {
            return Err(ANOTHER_RUN_IS_ACTIVE.into());
        }
        match progress_launch_id {
            Some(launch_id) => {
                let pending = pending_progress_launch_mut(&mut state, launch_id)?;
                if !matches!(pending.phase, ProgressLaunchPhase::Ready) {
                    return Err(
                        "The progress window has not acknowledged this synchronization launch"
                            .into(),
                    );
                }
                state.pending_progress_launch = None;
            }
            None if state.pending_progress_launch.is_some() => {
                return Err(LAUNCH_IS_ALREADY_PREPARING.into())
            }
            None => {}
        }
        state.next_run_id = state.next_run_id.checked_add(1).ok_or_else(|| {
            "The run identifier space is exhausted — restart SyncDash".to_string()
        })?;
        state.post_run_power_action_grant = None;
        let run_id = state.next_run_id;
        let control = RunCtl::new();
        state.active_run = Some(ActiveRun {
            run_id,
            purpose,
            control: control.clone(),
        });
        Ok(ActiveRunLease {
            lifecycle: self.clone(),
            run_id,
            control,
        })
    }
}
