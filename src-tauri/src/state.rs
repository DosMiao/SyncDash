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
use syncdash::model::plan::{Action, Op};
use syncdash::obs::progress::RunCtl;

use crate::dto::{CompareOwner, SelectedRowDto};

#[derive(Clone, Debug)]
pub(crate) struct CompareProvenance {
    pub(crate) owner: CompareOwner,
    pub(crate) plan_digest: String,
}

pub(crate) struct CachedSnaps {
    pub(crate) provenance: CompareProvenance,
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
/// 1:N: resolve a multi-target job into "the single-job view of the currently selected target"
/// (the engine's single pipeline is reused as-is). Return the normalized index with it so every
/// caller binds provenance to the same target when the optional argument is absent.
pub(crate) fn resolve_target(job: &job::Job, target_index: Option<usize>) -> Result<(usize, job::Job), String> {
    job.validate_multi_target()?;
    let list = job.target_list();
    let idx = target_index.unwrap_or(0);
    let t = list.get(idx).ok_or_else(|| format!("target index {idx} is out of range ({} total)", list.len()))?;
    Ok((idx, job.for_target(t)))
}

/// Prove that a submitted result is still the exact successful compare cached by this process.
/// Invocation context is checked before the cache, giving a useful reason when the job or target
/// changed; the exact cached owner and digest then prevent stale or client-mutated plans from use.
pub(crate) fn validate_cached_compare(
    cached: Option<&CompareProvenance>,
    owner: &CompareOwner,
    job_name: &str,
    target_index: usize,
    config_revision: &str,
    plan_digest: Option<&str>,
) -> Result<(), String> {
    if owner.job_name != job_name {
        return Err(format!(
            "This compare result belongs to job '{}', not '{}' — run Compare again",
            owner.job_name, job_name
        ));
    }
    if owner.target_index != target_index {
        return Err(format!(
            "This compare result belongs to target {}, not target {} — run Compare again",
            owner.target_index + 1,
            target_index + 1
        ));
    }
    if owner.config_revision != config_revision {
        return Err(format!("Job '{job_name}' changed since this compare — run Compare again"));
    }
    let Some(cached) = cached else {
        return Err("This compare result is no longer cached — run Compare again".into());
    };
    if cached.owner != *owner {
        return Err("A newer compare result replaced this one — run Compare again".into());
    }
    if let Some(plan_digest) = plan_digest {
        if cached.plan_digest != plan_digest {
            return Err(
                "This compare result no longer matches the plan produced by Compare — run Compare again"
                    .into(),
            );
        }
    }
    Ok(())
}

/// Rebuild the executable subset from the authenticated plan. IPC carries only row indices and the
/// user's flip choice, so it cannot smuggle in a path/action that Compare never produced.
pub(crate) fn resolve_selected_ops(
    plan_ops: &[Op],
    selected: &[SelectedRowDto],
) -> Result<Vec<Op>, String> {
    // An empty apply is not a harmless no-op in sync mode: the runners refresh the archive after a
    // completed run. Reject it here, before preflight or run reservation, so a report-only compare
    // (Conflict/Note rows) cannot mutate synchronization state through AutoScan.
    if selected.is_empty() {
        return Err("No executable rows were selected — review this compare result first".into());
    }
    let mut decisions = vec![None; plan_ops.len()];
    for row in selected {
        if row.index >= plan_ops.len() {
            return Err(format!(
                "Selected row {} is outside this compare result — run Compare again",
                row.index + 1
            ));
        }
        if decisions[row.index].is_some() {
            return Err(format!(
                "Selected row {} was submitted more than once — run Compare again",
                row.index + 1
            ));
        }
        decisions[row.index] = Some(row.flipped);
    }

    let mut out = Vec::with_capacity(selected.len());
    for (index, flipped) in decisions.into_iter().enumerate() {
        let Some(flipped) = flipped else { continue };
        let original = &plan_ops[index];
        let op = if flipped {
            syncdash::pipeline::compare::evidence::reverse_op(original).ok_or_else(|| {
                format!(
                    "Selected row {} cannot be reversed — run Compare again",
                    index + 1
                )
            })?
        } else {
            if matches!(original.action, Action::Conflict | Action::Note) {
                return Err(format!(
                    "Selected row {} is a report, not an operation — run Compare again",
                    index + 1
                ));
            }
            original.clone()
        };
        out.push(op);
    }
    Ok(out)
}

pub(crate) fn user_err(e: std::io::Error) -> String {
    if syncdash::obs::progress::is_cancelled(&e) { "cancelled".into() } else { e.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syncdash::model::plan::Side;

    fn owner(compare_id: u64) -> CompareOwner {
        CompareOwner {
            compare_id,
            job_name: "photos".into(),
            target_index: 1,
            config_revision: "revision-a".into(),
        }
    }

    fn provenance(compare_id: u64) -> CompareProvenance {
        CompareProvenance { owner: owner(compare_id), plan_digest: "plan-a".into() }
    }

    fn op(action: Action, path: &str) -> Op {
        Op {
            side: Side::Target,
            action,
            path: path.into(),
            from: None,
            size: Some(12),
            mtime_ms: Some(34),
            hash: Some("hash".into()),
            link: None,
            mode: None,
            reason: "compared".into(),
        }
    }

    #[test]
    fn cached_compare_requires_exact_owner_and_original_plan() {
        let cached = provenance(7);
        assert!(validate_cached_compare(
            Some(&cached),
            &owner(7),
            "photos",
            1,
            "revision-a",
            Some("plan-a"),
        )
        .is_ok());

        let newer = validate_cached_compare(
            Some(&provenance(8)),
            &owner(7),
            "photos",
            1,
            "revision-a",
            Some("plan-a"),
        )
        .unwrap_err();
        assert!(newer.contains("newer compare"), "{newer}");

        let mutated = validate_cached_compare(
            Some(&cached),
            &owner(7),
            "photos",
            1,
            "revision-a",
            Some("plan-b"),
        )
        .unwrap_err();
        assert!(mutated.contains("no longer matches"), "{mutated}");
    }

    #[test]
    fn cached_compare_cannot_be_reinterpreted_as_another_job_target_or_revision() {
        let cached = provenance(7);
        let wrong_job = validate_cached_compare(
            Some(&cached),
            &owner(7),
            "documents",
            1,
            "revision-a",
            None,
        )
        .unwrap_err();
        assert!(wrong_job.contains("belongs to job"), "{wrong_job}");

        let wrong_target = validate_cached_compare(
            Some(&cached),
            &owner(7),
            "photos",
            0,
            "revision-a",
            None,
        )
        .unwrap_err();
        assert!(wrong_target.contains("belongs to target"), "{wrong_target}");

        let changed_job = validate_cached_compare(
            Some(&cached),
            &owner(7),
            "photos",
            1,
            "revision-b",
            None,
        )
        .unwrap_err();
        assert!(changed_job.contains("changed since"), "{changed_job}");

        let gone = validate_cached_compare(
            None,
            &owner(7),
            "photos",
            1,
            "revision-a",
            None,
        )
        .unwrap_err();
        assert!(gone.contains("no longer cached"), "{gone}");
    }

    #[test]
    fn selected_rows_are_reconstructed_from_the_plan_including_valid_reversals() {
        let plan = [op(Action::Copy, "safe/file.txt"), op(Action::Update, "other.txt")];
        let selected = [
            SelectedRowDto { index: 1, flipped: false },
            SelectedRowDto { index: 0, flipped: true },
        ];
        let resolved = resolve_selected_ops(&plan, &selected).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].path, "safe/file.txt");
        assert!(matches!(resolved[0].action, Action::Delete));
        assert_eq!(resolved[1].path, "other.txt");
        assert!(matches!(resolved[1].action, Action::Update));
    }

    #[test]
    fn selected_rows_reject_injection_duplicates_and_non_operations() {
        let plan = [op(Action::Copy, "safe/file.txt"), op(Action::Conflict, "conflict.txt")];

        let empty = resolve_selected_ops(&plan, &[]).unwrap_err();
        assert!(empty.contains("No executable rows"), "{empty}");

        let outside = resolve_selected_ops(&plan, &[SelectedRowDto { index: 2, flipped: false }])
            .unwrap_err();
        assert!(outside.contains("outside"), "{outside}");

        let duplicate = resolve_selected_ops(
            &plan,
            &[
                SelectedRowDto { index: 0, flipped: false },
                SelectedRowDto { index: 0, flipped: true },
            ],
        )
        .unwrap_err();
        assert!(duplicate.contains("more than once"), "{duplicate}");

        let report = resolve_selected_ops(&plan, &[SelectedRowDto { index: 1, flipped: false }])
            .unwrap_err();
        assert!(report.contains("not an operation"), "{report}");
    }

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
