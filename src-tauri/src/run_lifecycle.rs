//! Process-wide lifecycle for run commands, progress launches, and the one active engine run.

use std::sync::{Arc, Mutex};

use syncdash::obs::progress::RunCtl;

#[derive(Clone)]
struct ActiveRun {
    run_id: u64,
    purpose: RunPurpose,
    control: Arc<RunCtl>,
}

#[derive(Default)]
struct LifecycleState {
    active_run: Option<ActiveRun>,
    pending_progress_launch_id: Option<u64>,
    progress_window_closing: bool,
    commands_in_flight: u64,
    registry_mutation_in_progress: bool,
    next_run_id: u64,
    next_progress_launch_id: u64,
}

#[derive(Default)]
pub(crate) struct RunLifecycle {
    state: Mutex<LifecycleState>,
}

pub(crate) struct RunCommandLease {
    lifecycle: Arc<RunLifecycle>,
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
    lifecycle: Arc<RunLifecycle>,
    run_id: u64,
    control: Arc<RunCtl>,
}

impl ActiveRunLease {
    pub(crate) fn run_id(&self) -> u64 {
        self.run_id
    }

    pub(crate) fn control(&self) -> Arc<RunCtl> {
        self.control.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunPurpose {
    Compare,
    Apply,
}

struct RegistryMutationLease<'a> {
    lifecycle: &'a RunLifecycle,
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

impl Drop for ActiveRunLease {
    fn drop(&mut self) {
        let mut state = self.lifecycle.state.lock().unwrap();
        if state.active_run.as_ref().map(|run| run.run_id) == Some(self.run_id) {
            state.active_run = None;
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "decision", rename_all = "snake_case")]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) enum ProgressWindowCloseDecisionDto {
    PendingLaunchCancelled,
    ActiveRunCancellationRequested {
        #[ts(type = "number")]
        run_id: u64,
    },
    NoInteractiveLaunch,
}

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

    fn start_run(
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
            Some(launch_id) if state.pending_progress_launch_id == Some(launch_id) => {
                state.pending_progress_launch_id = None;
            }
            Some(_) => return Err("This synchronization launch is no longer active".into()),
            None if state.pending_progress_launch_id.is_some() => {
                return Err("A synchronization is already preparing to start".into())
            }
            None => {}
        }
        state.next_run_id = state.next_run_id.checked_add(1).ok_or_else(|| {
            "The run identifier space is exhausted — restart SyncDash".to_string()
        })?;
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

    pub(crate) fn reserve_progress_launch(&self) -> Result<u64, String> {
        let mut state = self.state.lock().unwrap();
        if state.progress_window_closing {
            return Err("The progress window is closing — wait a moment and try again".into());
        }
        if state.commands_in_flight > 0 || state.registry_mutation_in_progress {
            return Err(
                "Another operation is preparing — wait for its review or preflight to finish"
                    .into(),
            );
        }
        if state.active_run.is_some() {
            return Err(
                "Another run is already in progress — cancel it or wait for it to finish".into(),
            );
        }
        if state.pending_progress_launch_id.is_some() {
            return Err("A synchronization is already preparing to start".into());
        }
        state.next_progress_launch_id =
            state
                .next_progress_launch_id
                .checked_add(1)
                .ok_or_else(|| {
                    "The progress launch identifier space is exhausted — restart SyncDash"
                        .to_string()
                })?;
        let launch_id = state.next_progress_launch_id;
        state.pending_progress_launch_id = Some(launch_id);
        Ok(launch_id)
    }

    pub(crate) fn cancel_progress_launch(&self, launch_id: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.pending_progress_launch_id != Some(launch_id) {
            return false;
        }
        state.pending_progress_launch_id = None;
        true
    }

    pub(crate) fn begin_progress_window_close(&self) -> ProgressWindowCloseDecisionDto {
        let mut state = self.state.lock().unwrap();
        state.progress_window_closing = true;
        if state.pending_progress_launch_id.take().is_some() {
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
        self.state.lock().unwrap().progress_window_closing = false;
    }

    pub(crate) fn has_activity(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.active_run.is_some()
            || state.pending_progress_launch_id.is_some()
            || state.commands_in_flight > 0
            || state.registry_mutation_in_progress
    }

    pub(crate) fn with_idle_mutation<T>(
        &self,
        operation: &str,
        mutate: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state = self.state.lock().unwrap();
        if state.active_run.is_some()
            || state.pending_progress_launch_id.is_some()
            || state.commands_in_flight > 0
            || state.registry_mutation_in_progress
        {
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

    pub(crate) fn request_cancel(&self, run_id: u64) -> Result<bool, String> {
        let state = self.state.lock().unwrap();
        let Some(active) = state.active_run.as_ref() else {
            return Ok(false);
        };
        if active.run_id != run_id {
            return Err(format!(
                "Run {run_id} is no longer active; run {} is active now",
                active.run_id
            ));
        }
        active.control.request_cancel();
        Ok(true)
    }

    pub(crate) fn set_paused(&self, run_id: u64, paused: bool) -> Result<bool, String> {
        let state = self.state.lock().unwrap();
        let Some(active) = state.active_run.as_ref() else {
            return Ok(false);
        };
        if active.run_id != run_id {
            return Err(format!(
                "Run {run_id} is no longer active; run {} is active now",
                active.run_id
            ));
        }
        active.control.set_paused(paused);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_launch_is_reserved_and_consumed_exactly_once() {
        let lifecycle = Arc::new(RunLifecycle::default());
        let launch_id = lifecycle.reserve_progress_launch().unwrap();
        assert!(lifecycle.reserve_progress_launch().is_err());
        let command = lifecycle.command_lease().unwrap();
        assert!(command.start_run(RunPurpose::Compare).is_err());
        assert!(command
            .start_apply_from_progress_launch(launch_id + 1)
            .is_err());

        let run = command.start_apply_from_progress_launch(launch_id).unwrap();
        assert_eq!(run.run_id(), 1);
        assert_eq!(
            lifecycle.begin_progress_window_close(),
            ProgressWindowCloseDecisionDto::ActiveRunCancellationRequested { run_id: 1 }
        );
        assert!(run.control().cancelled());
        drop(run);
        drop(command);
        assert!(!lifecycle.has_activity());
    }

    #[test]
    fn closing_window_blocks_new_launches_until_destruction_finishes() {
        let lifecycle = RunLifecycle::default();
        assert_eq!(
            lifecycle.begin_progress_window_close(),
            ProgressWindowCloseDecisionDto::NoInteractiveLaunch
        );
        assert!(lifecycle.reserve_progress_launch().is_err());
        lifecycle.finish_progress_window_close();
        assert!(lifecycle.reserve_progress_launch().is_ok());
    }

    #[test]
    fn pending_launch_close_invalidates_only_that_launch() {
        let lifecycle = RunLifecycle::default();
        let launch_id = lifecycle.reserve_progress_launch().unwrap();
        assert!(!lifecycle.cancel_progress_launch(launch_id + 1));
        assert_eq!(
            lifecycle.begin_progress_window_close(),
            ProgressWindowCloseDecisionDto::PendingLaunchCancelled
        );
        assert!(!lifecycle.cancel_progress_launch(launch_id));
    }

    #[test]
    fn closing_an_open_progress_window_cancels_an_unattended_apply() {
        let lifecycle = Arc::new(RunLifecycle::default());
        let command = lifecycle.command_lease().unwrap();
        let run = command.start_run(RunPurpose::Apply).unwrap();

        assert_eq!(
            lifecycle.begin_progress_window_close(),
            ProgressWindowCloseDecisionDto::ActiveRunCancellationRequested { run_id: 1 }
        );
        assert!(run.control().cancelled());
    }

    #[test]
    fn delayed_controls_cannot_target_a_newer_run() {
        let lifecycle = Arc::new(RunLifecycle::default());
        let command = lifecycle.command_lease().unwrap();
        let first = command.start_run(RunPurpose::Compare).unwrap();
        let first_id = first.run_id();
        drop(first);
        let second = command.start_run(RunPurpose::Apply).unwrap();
        let second_id = second.run_id();

        assert!(lifecycle.request_cancel(first_id).is_err());
        assert!(lifecycle.set_paused(first_id, true).is_err());
        assert!(lifecycle.set_paused(second_id, true).unwrap());
        assert!(lifecycle.request_cancel(second_id).unwrap());
        assert!(second.control().cancelled());
    }

    #[test]
    fn command_and_active_run_leases_release_during_unwind() {
        let lifecycle = Arc::new(RunLifecycle::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let lifecycle = lifecycle.clone();
            move || {
                let command = lifecycle.command_lease().unwrap();
                let _run = command.start_run(RunPurpose::Compare).unwrap();
                panic!("worker panic");
            }
        }));

        assert!(result.is_err());
        assert!(!lifecycle.has_activity());
        let command = lifecycle.command_lease().unwrap();
        assert!(command.start_run(RunPurpose::Compare).is_ok());
    }

    #[test]
    fn idle_mutation_excludes_every_command_and_run_state() {
        let lifecycle = Arc::new(RunLifecycle::default());
        let command = lifecycle.command_lease().unwrap();
        let error = lifecycle
            .with_idle_mutation("Saving jobs", || Ok(()))
            .unwrap_err();
        assert!(error.contains("unavailable"), "{error}");
        drop(command);

        assert_eq!(
            lifecycle
                .with_idle_mutation("Saving jobs", || {
                    assert!(lifecycle.command_lease().is_err());
                    assert!(lifecycle.reserve_progress_launch().is_err());
                    Ok(42)
                })
                .unwrap(),
            42
        );
        assert!(lifecycle.command_lease().is_ok());
    }

    #[test]
    fn preparing_command_blocks_progress_launch_reservation() {
        let lifecycle = Arc::new(RunLifecycle::default());
        let command = lifecycle.command_lease().unwrap();
        assert!(lifecycle.reserve_progress_launch().is_err());
        drop(command);
        assert!(lifecycle.reserve_progress_launch().is_ok());
    }
}
