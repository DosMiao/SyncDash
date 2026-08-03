//! Internal state for active runs, command exclusion, and progress-launch handshakes.

use std::sync::mpsc::SyncSender;
use std::sync::Arc;

use syncdash::obs::progress::RunCtl;

use super::model::RunPurpose;

#[derive(Clone)]
pub(super) struct ActiveRun {
    pub(super) run_id: u64,
    pub(super) purpose: RunPurpose,
    pub(super) control: Arc<RunCtl>,
}

#[derive(Default)]
pub(super) struct LifecycleState {
    pub(super) active_run: Option<ActiveRun>,
    pub(super) pending_progress_launch: Option<PendingProgressLaunch>,
    pub(super) progress_window_closing: bool,
    pub(super) commands_in_flight: u64,
    pub(super) registry_mutation_in_progress: bool,
    pub(super) post_run_power_action_grant: Option<u64>,
    pub(super) next_run_id: u64,
    pub(super) next_progress_launch_id: u64,
}

impl LifecycleState {
    /// Anything that must finish before the registry or settings may be mutated and before the
    /// main window may close: a run, a reserved progress launch, an in-flight run command, or a
    /// mutation already holding the lease.
    pub(super) fn is_busy(&self) -> bool {
        self.active_run.is_some()
            || self.pending_progress_launch.is_some()
            || self.commands_in_flight > 0
            || self.registry_mutation_in_progress
    }
}

/// Refusals shared by launch reservation and run admission, which check the same state from two
/// entry points and must not drift into two different explanations of the same situation.
pub(super) const ANOTHER_RUN_IS_ACTIVE: &str =
    "Another run is already in progress — cancel it or wait for it to finish";
pub(super) const LAUNCH_IS_NO_LONGER_ACTIVE: &str =
    "This synchronization launch is no longer active";
pub(super) const LAUNCH_IS_ALREADY_PREPARING: &str =
    "A synchronization is already preparing to start";

pub(super) struct PendingProgressLaunch {
    pub(super) id: u64,
    pub(super) phase: ProgressLaunchPhase,
}

pub(super) enum ProgressLaunchPhase {
    Reserved,
    AwaitingWindowMount(SyncSender<()>),
    WindowMounted,
    AwaitingLaunchAcknowledgement(SyncSender<()>),
    Ready,
}
