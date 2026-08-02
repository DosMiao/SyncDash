//! Reservation and ordered handshakes for one progress-window launch.

use std::sync::mpsc::{self, Receiver};

use super::coordinator::RunLifecycle;
use super::state::{LifecycleState, PendingProgressLaunch, ProgressLaunchPhase};

impl RunLifecycle {
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
