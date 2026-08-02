//! Command admission and active-run creation.

use std::sync::Arc;

use syncdash::obs::progress::RunCtl;

use super::coordinator::RunLifecycle;
use super::lease::{ActiveRunLease, RunCommandLease};
use super::model::RunPurpose;
use super::state::{ActiveRun, ProgressLaunchPhase};

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
            return Err(
                "Another run is already in progress — cancel it or wait for it to finish".into(),
            );
        }
        match progress_launch_id {
            Some(launch_id) => {
                let Some(pending) = state.pending_progress_launch.as_ref() else {
                    return Err("This synchronization launch is no longer active".into());
                };
                if pending.id != launch_id {
                    return Err("This synchronization launch is no longer active".into());
                }
                if !matches!(pending.phase, ProgressLaunchPhase::Ready) {
                    return Err(
                        "The progress window has not acknowledged this synchronization launch"
                            .into(),
                    );
                }
                state.pending_progress_launch = None;
            }
            None if state.pending_progress_launch.is_some() => {
                return Err("A synchronization is already preparing to start".into())
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
