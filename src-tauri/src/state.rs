//! Process-wide run state.
//!
//! One run at a time, so cancel and pause always have an unambiguous target. An interactive apply
//! reserves its launch before opening the progress window; consuming that same ID is part of
//! beginning the run, so duplicate starts and pre-start window closes cannot race it. The snapshot
//! cache is a single slot: compare already walked both sides in full, and dropping the snapshots
//! would make the "Identical" panel rescan just to be looked at.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use syncdash::job::{self};
use syncdash::obs::progress::RunCtl;

pub(crate) struct CachedSnaps {
    pub(crate) job: String,
    pub(crate) source: syncdash::model::table::Snapshot,
    pub(crate) target: syncdash::model::table::Snapshot,
}

#[derive(Default)]
pub(crate) struct SnapCache(pub(crate) Mutex<Option<CachedSnaps>>);
#[derive(Default)]
pub(crate) struct RunState {
    gate: Mutex<()>,
    pub(crate) active: Mutex<Option<Arc<RunCtl>>>,
    active_launch: Mutex<Option<u64>>,
    pending_launch: Mutex<Option<u64>>,
    progress_window_closing: Mutex<bool>,
    pub(crate) seq: AtomicU64,
    launch_seq: AtomicU64,
}

pub(crate) fn begin_run(st: &RunState) -> Result<(u64, Arc<RunCtl>), String> {
    begin_run_for_launch(st, None)
}

pub(crate) fn reserve_progress_launch(st: &RunState) -> Result<u64, String> {
    let _gate = st.gate.lock().unwrap();
    if *st.progress_window_closing.lock().unwrap() {
        return Err("The progress window is closing — wait a moment and try again".into());
    }
    if st.active.lock().unwrap().is_some() {
        return Err("Another run is already in progress — cancel it or wait for it to finish".into());
    }
    let mut pending = st.pending_launch.lock().unwrap();
    if pending.is_some() {
        return Err("A synchronization is already preparing to start".into());
    }
    let launch_id = st.launch_seq.fetch_add(1, Ordering::Relaxed) + 1;
    *pending = Some(launch_id);
    Ok(launch_id)
}

pub(crate) fn release_progress_launch(st: &RunState, launch_id: u64) -> bool {
    let _gate = st.gate.lock().unwrap();
    let mut pending = st.pending_launch.lock().unwrap();
    if *pending != Some(launch_id) {
        return false;
    }
    *pending = None;
    true
}

pub(crate) fn close_progress_launch(st: &RunState) -> &'static str {
    let _gate = st.gate.lock().unwrap();
    *st.progress_window_closing.lock().unwrap() = true;
    let mut pending = st.pending_launch.lock().unwrap();
    if pending.take().is_some() {
        return "pending";
    }
    if st.active_launch.lock().unwrap().is_some() {
        if let Some(ctl) = st.active.lock().unwrap().as_ref() {
            ctl.request_cancel();
        }
        return "active";
    }
    "none"
}

/// Release the close barrier only after Tauri has destroyed the old webview. While it is set, a
/// new launch cannot acknowledge that old window in the gap between the close check and teardown.
pub(crate) fn finish_progress_window_close(st: &RunState) {
    let _gate = st.gate.lock().unwrap();
    *st.progress_window_closing.lock().unwrap() = false;
}

pub(crate) fn begin_run_for_launch(
    st: &RunState,
    launch_id: Option<u64>,
) -> Result<(u64, Arc<RunCtl>), String> {
    let _gate = st.gate.lock().unwrap();
    let mut g = st.active.lock().unwrap();
    if g.is_some() {
        return Err("Another run is already in progress — cancel it or wait for it to finish".into());
    }
    let mut pending = st.pending_launch.lock().unwrap();
    match launch_id {
        Some(id) if *pending == Some(id) => *pending = None,
        Some(_) => return Err("This synchronization launch is no longer active".into()),
        None if pending.is_some() => return Err("A synchronization is already preparing to start".into()),
        None => {}
    }
    let ctl = RunCtl::new();
    *g = Some(ctl.clone());
    *st.active_launch.lock().unwrap() = launch_id;
    Ok((st.seq.fetch_add(1, Ordering::Relaxed) + 1, ctl))
}

pub(crate) fn end_run(st: &RunState) {
    let _gate = st.gate.lock().unwrap();
    *st.active.lock().unwrap() = None;
    *st.active_launch.lock().unwrap() = None;
}

// Event bridge
/// 1:N: resolve a multi-target job into "the single-job view of the currently selected target" (the engine's single pipeline is reused as-is)
pub(crate) fn resolve_target(job: &job::Job, target_index: Option<usize>) -> Result<job::Job, String> {
    job.validate_multi_target()?;
    let list = job.target_list();
    let idx = target_index.unwrap_or(0);
    let t = list.get(idx).ok_or_else(|| format!("target index {idx} is out of range ({} total)", list.len()))?;
    Ok(job.for_target(t))
}

pub(crate) fn user_err(e: std::io::Error) -> String {
    if syncdash::obs::progress::is_cancelled(&e) { "cancelled".into() } else { e.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_progress_launch_reserves_exactly_one_run() {
        let state = RunState::default();
        let launch = reserve_progress_launch(&state).unwrap();
        assert!(reserve_progress_launch(&state).is_err());
        assert!(begin_run(&state).is_err());
        assert!(begin_run_for_launch(&state, Some(launch + 1)).is_err());

        let (_, ctl) = begin_run_for_launch(&state, Some(launch)).unwrap();
        assert!(Arc::ptr_eq(state.active.lock().unwrap().as_ref().unwrap(), &ctl));
        assert_eq!(close_progress_launch(&state), "active");
        assert!(ctl.cancelled());
        end_run(&state);
        assert!(state.active.lock().unwrap().is_none());
    }

    #[test]
    fn only_the_matching_launch_can_be_released() {
        let state = RunState::default();
        let launch = reserve_progress_launch(&state).unwrap();
        assert!(!release_progress_launch(&state, launch + 1));
        assert_eq!(close_progress_launch(&state), "pending");
        assert!(!release_progress_launch(&state, launch));
        assert!(begin_run(&state).is_ok());
    }

    #[test]
    fn closing_window_blocks_a_new_reservation_until_destroyed() {
        let state = RunState::default();
        assert_eq!(close_progress_launch(&state), "none");
        assert!(reserve_progress_launch(&state).is_err());

        finish_progress_window_close(&state);
        assert!(reserve_progress_launch(&state).is_ok());
    }
}

// Commands
