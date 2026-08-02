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
