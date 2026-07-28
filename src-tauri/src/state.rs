//! Process-wide run state.
//!
//! One run at a time, so cancel and pause always have an unambiguous target. The snapshot cache is
//! a single slot: compare already walked both sides in full, and dropping the snapshots would make
//! the "Identical" panel rescan just to be looked at.

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
    pub(crate) active: Mutex<Option<Arc<RunCtl>>>,
    pub(crate) seq: AtomicU64,
}
pub(crate) fn begin_run(st: &RunState) -> Result<(u64, Arc<RunCtl>), String> {
    let mut g = st.active.lock().unwrap();
    if g.is_some() {
        return Err("Another run is already in progress — cancel it or wait for it to finish".into());
    }
    let ctl = RunCtl::new();
    *g = Some(ctl.clone());
    Ok((st.seq.fetch_add(1, Ordering::Relaxed) + 1, ctl))
}
pub(crate) fn end_run(st: &RunState) {
    *st.active.lock().unwrap() = None;
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

// Commands
