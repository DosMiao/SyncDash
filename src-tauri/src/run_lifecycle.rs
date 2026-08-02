//! Process-wide lifecycle for run commands, progress launches, and the one active engine run.

use std::sync::mpsc::{self, Receiver, SyncSender};
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
    pending_progress_launch: Option<PendingProgressLaunch>,
    progress_window_closing: bool,
    commands_in_flight: u64,
    registry_mutation_in_progress: bool,
    post_run_power_action_grant: Option<u64>,
    next_run_id: u64,
    next_progress_launch_id: u64,
}

struct PendingProgressLaunch {
    id: u64,
    phase: ProgressLaunchPhase,
}

enum ProgressLaunchPhase {
    Reserved,
    AwaitingWindowMount(SyncSender<()>),
    WindowMounted,
    AwaitingLaunchAcknowledgement(SyncSender<()>),
    Ready,
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

impl RunPurpose {
    const fn name(self) -> &'static str {
        match self {
            Self::Compare => "Compare",
            Self::Apply => "Apply",
        }
    }
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
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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
        if state.pending_progress_launch.is_some() {
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
        state.post_run_power_action_grant = None;
        let launch_id = state.next_progress_launch_id;
        state.pending_progress_launch = Some(PendingProgressLaunch {
            id: launch_id,
            phase: ProgressLaunchPhase::Reserved,
        });
        Ok(launch_id)
    }

    pub(crate) fn prepare_progress_window_mount(
        &self,
        launch_id: u64,
    ) -> Result<Receiver<()>, String> {
        let mut state = self.state.lock().unwrap();
        let pending = pending_progress_launch_mut(&mut state, launch_id)?;
        if !matches!(pending.phase, ProgressLaunchPhase::Reserved) {
            return Err("The progress window mount handshake is out of sequence".into());
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        pending.phase = ProgressLaunchPhase::AwaitingWindowMount(sender);
        Ok(receiver)
    }

    pub(crate) fn report_progress_window_mounted(&self, launch_id: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let pending = pending_progress_launch_mut(&mut state, launch_id)?;
        let ProgressLaunchPhase::AwaitingWindowMount(sender) = &pending.phase else {
            return Err("The progress window mount handshake is out of sequence".into());
        };
        sender
            .try_send(())
            .map_err(|_| "The progress window mount handshake is no longer active".to_string())?;
        pending.phase = ProgressLaunchPhase::WindowMounted;
        Ok(())
    }

    pub(crate) fn prepare_progress_launch_acknowledgement(
        &self,
        launch_id: u64,
    ) -> Result<Receiver<()>, String> {
        let mut state = self.state.lock().unwrap();
        let pending = pending_progress_launch_mut(&mut state, launch_id)?;
        if !matches!(
            pending.phase,
            ProgressLaunchPhase::Reserved | ProgressLaunchPhase::WindowMounted
        ) {
            return Err("The progress launch acknowledgement is out of sequence".into());
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        pending.phase = ProgressLaunchPhase::AwaitingLaunchAcknowledgement(sender);
        Ok(receiver)
    }

    pub(crate) fn acknowledge_progress_launch(&self, launch_id: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let pending = pending_progress_launch_mut(&mut state, launch_id)?;
        let ProgressLaunchPhase::AwaitingLaunchAcknowledgement(sender) = &pending.phase else {
            return Err("The progress launch acknowledgement is out of sequence".into());
        };
        sender
            .try_send(())
            .map_err(|_| "The progress launch acknowledgement is no longer active".to_string())?;
        pending.phase = ProgressLaunchPhase::Ready;
        Ok(())
    }

    pub(crate) fn cancel_progress_launch(&self, launch_id: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if state
            .pending_progress_launch
            .as_ref()
            .map(|launch| launch.id)
            != Some(launch_id)
        {
            return false;
        }
        state.pending_progress_launch = None;
        true
    }

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
        let state = self.state.lock().unwrap();
        state.active_run.is_some()
            || state.pending_progress_launch.is_some()
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
            || state.pending_progress_launch.is_some()
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

    pub(crate) fn request_cancel(
        &self,
        run_id: u64,
        expected_purpose: RunPurpose,
    ) -> Result<bool, String> {
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
        if active.purpose != expected_purpose {
            return Err(format!(
                "Run {run_id} is an {} run and cannot be controlled as {}",
                active.purpose.name(),
                expected_purpose.name()
            ));
        }
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
        let Some(active) = state.active_run.as_ref() else {
            return Ok(false);
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

fn pending_progress_launch_mut(
    state: &mut LifecycleState,
    launch_id: u64,
) -> Result<&mut PendingProgressLaunch, String> {
    let Some(pending) = state.pending_progress_launch.as_mut() else {
        return Err("This synchronization launch is no longer active".into());
    };
    if pending.id != launch_id {
        return Err("This synchronization launch is no longer active".into());
    }
    Ok(pending)
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
        assert!(command.start_apply_from_progress_launch(launch_id).is_err());

        let ready = lifecycle
            .prepare_progress_launch_acknowledgement(launch_id)
            .unwrap();
        lifecycle.acknowledge_progress_launch(launch_id).unwrap();
        ready.recv().unwrap();

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

        assert!(lifecycle
            .request_cancel(first_id, RunPurpose::Compare)
            .is_err());
        assert!(lifecycle
            .set_paused(first_id, RunPurpose::Apply, true)
            .is_err());
        assert!(lifecycle
            .request_cancel(second_id, RunPurpose::Compare)
            .is_err());
        assert!(lifecycle
            .set_paused(second_id, RunPurpose::Apply, true)
            .unwrap());
        assert!(lifecycle
            .request_cancel(second_id, RunPurpose::Apply)
            .unwrap());
        assert!(second.control().cancelled());
    }

    #[test]
    fn progress_handshake_binds_mount_and_readiness_to_one_launch() {
        let lifecycle = RunLifecycle::default();
        let launch_id = lifecycle.reserve_progress_launch().unwrap();
        assert!(lifecycle.report_progress_window_mounted(launch_id).is_err());

        let mounted = lifecycle.prepare_progress_window_mount(launch_id).unwrap();
        assert!(lifecycle
            .report_progress_window_mounted(launch_id + 1)
            .is_err());
        lifecycle.report_progress_window_mounted(launch_id).unwrap();
        mounted.recv().unwrap();

        let ready = lifecycle
            .prepare_progress_launch_acknowledgement(launch_id)
            .unwrap();
        assert!(lifecycle
            .acknowledge_progress_launch(launch_id + 1)
            .is_err());
        lifecycle.acknowledge_progress_launch(launch_id).unwrap();
        ready.recv().unwrap();
        assert!(lifecycle.acknowledge_progress_launch(launch_id).is_err());
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

    #[test]
    fn successful_apply_grant_is_exact_retryable_and_one_use() {
        let lifecycle = Arc::new(RunLifecycle::default());
        let command = lifecycle.command_lease().unwrap();
        let run = command.start_run(RunPurpose::Apply).unwrap();
        let run_id = run.run_id();
        lifecycle.issue_post_run_power_action_grant(run_id).unwrap();
        drop(run);
        drop(command);

        assert!(lifecycle
            .consume_post_run_power_action_grant_with(run_id + 1, || Ok(()))
            .is_err());
        assert!(lifecycle
            .consume_post_run_power_action_grant_with::<()>(run_id, || {
                Err("system command unavailable".into())
            })
            .is_err());
        assert!(lifecycle
            .consume_post_run_power_action_grant_with(run_id, || Ok(()))
            .is_ok());
        assert!(lifecycle
            .consume_post_run_power_action_grant_with(run_id, || Ok(()))
            .is_err());
    }

    #[test]
    fn compare_and_a_new_launch_cannot_reuse_a_power_action_grant() {
        let lifecycle = Arc::new(RunLifecycle::default());
        let command = lifecycle.command_lease().unwrap();
        let compare = command.start_run(RunPurpose::Compare).unwrap();
        assert!(lifecycle
            .issue_post_run_power_action_grant(compare.run_id())
            .is_err());
        drop(compare);
        let apply = command.start_run(RunPurpose::Apply).unwrap();
        let apply_id = apply.run_id();
        lifecycle
            .issue_post_run_power_action_grant(apply_id)
            .unwrap();
        drop(apply);
        drop(command);

        lifecycle.reserve_progress_launch().unwrap();
        assert!(lifecycle
            .consume_post_run_power_action_grant_with(apply_id, || Ok(()))
            .is_err());
    }
}
