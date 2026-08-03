//! Active-run controls, idle mutation exclusion, progress-window close, and power grants.

use super::lease::RegistryMutationLease;
use super::model::{ProgressWindowCloseDecisionDto, RunPurpose};
use super::state::{ActiveRun, LifecycleState};
use super::RunLifecycle;

impl RunLifecycle {
    pub(crate) fn begin_progress_window_close(&self) -> ProgressWindowCloseDecisionDto {
        let mut state = self.state.lock().unwrap();
        state.progress_window_closing = true;
        state.post_run_power_action_grant = None;
        if state.pending_progress_launch.take().is_some() {
            return ProgressWindowCloseDecisionDto::PendingLaunchCancelled;
        }
        if let Some(active) = state
            .active_run
            .as_ref()
            .filter(|run| run.purpose == RunPurpose::Apply)
        {
            active.control.request_cancel();
            return ProgressWindowCloseDecisionDto::ActiveRunCancellationRequested {
                run_id: active.run_id,
            };
        }
        ProgressWindowCloseDecisionDto::NoInteractiveLaunch
    }

    pub(crate) fn finish_progress_window_close(&self) {
        let mut state = self.state.lock().unwrap();
        state.progress_window_closing = false;
        state.post_run_power_action_grant = None;
    }

    pub(crate) fn has_activity(&self) -> bool {
        self.state.lock().unwrap().is_busy()
    }

    pub(crate) fn with_idle_mutation<T>(
        &self,
        operation: &str,
        mutate: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state = self.state.lock().unwrap();
        if state.is_busy() {
            return Err(format!(
                "{operation} is unavailable while Compare or Synchronize is preparing or running"
            ));
        }
        state.registry_mutation_in_progress = true;
        let mutation_lease = RegistryMutationLease { lifecycle: self };
        drop(state);
        let result = mutate();
        drop(mutation_lease);
        result
    }

    pub(crate) fn request_cancel(
        &self,
        run_id: u64,
        expected_purpose: RunPurpose,
    ) -> Result<bool, String> {
        let state = self.state.lock().unwrap();
        let Some(active) = controlled_run(&state, run_id, expected_purpose)? else {
            return Ok(false);
        };
        active.control.request_cancel();
        Ok(true)
    }

    pub(crate) fn set_paused(
        &self,
        run_id: u64,
        expected_purpose: RunPurpose,
        paused: bool,
    ) -> Result<bool, String> {
        let state = self.state.lock().unwrap();
        let Some(active) = controlled_run(&state, run_id, expected_purpose)? else {
            return Ok(false);
        };
        active.control.set_paused(paused);
        Ok(true)
    }

    pub(crate) fn issue_post_run_power_action_grant(&self, run_id: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let Some(active) = state.active_run.as_ref() else {
            return Err("The completed Apply run is no longer active".into());
        };
        if active.run_id != run_id || active.purpose != RunPurpose::Apply {
            return Err("Only the exact active Apply run can authorize a power action".into());
        }
        state.post_run_power_action_grant = Some(run_id);
        Ok(())
    }

    pub(crate) fn consume_post_run_power_action_grant_with<T>(
        &self,
        run_id: u64,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state = self.state.lock().unwrap();
        if state.post_run_power_action_grant != Some(run_id) {
            return Err(
                "This run has no unused successful-Apply power-action authorization".into(),
            );
        }
        let result = action()?;
        state.post_run_power_action_grant = None;
        Ok(result)
    }

    pub(crate) fn revoke_post_run_power_action_grant(&self, run_id: u64) {
        let mut state = self.state.lock().unwrap();
        if state.post_run_power_action_grant == Some(run_id) {
            state.post_run_power_action_grant = None;
        }
    }
}

/// Resolve the active run only when it is exactly the run the caller believes it is controlling.
/// `Ok(None)` means nothing is running: a control click that lands after its run already finished
/// is a no-op, while a click that lands on a *different* run is an error rather than a cancel of
/// whatever happens to be running now.
fn controlled_run(
    state: &LifecycleState,
    run_id: u64,
    expected_purpose: RunPurpose,
) -> Result<Option<&ActiveRun>, String> {
    let Some(active) = state.active_run.as_ref() else {
        return Ok(None);
    };
    if active.run_id != run_id {
        return Err(format!(
            "Run {run_id} is no longer active; run {} is active now",
            active.run_id
        ));
    }
    if active.purpose != expected_purpose {
        return Err(format!(
            "Run {run_id} is an {} run and cannot be controlled as {}",
            active.purpose.name(),
            expected_purpose.name()
        ));
    }
    Ok(Some(active))
}
