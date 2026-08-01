//! Process-wide run state.
//!
//! One run at a time, so cancel and pause always have an unambiguous target. An interactive apply
//! reserves its launch before opening the progress window; consuming that same ID is part of
//! beginning the run, so duplicate starts and pre-start window closes cannot race it. Successful
//! Compare-result retention lives in `compare_results`; this module owns only run lifecycle state
//! and shared operation reconstruction helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use syncdash::job;
use syncdash::model::plan::{Action, Op};
use syncdash::obs::progress::RunCtl;

use crate::dto::SelectedRowDto;

#[derive(Clone)]
pub(crate) struct ActiveRun {
    pub(crate) id: u64,
    pub(crate) ctl: Arc<RunCtl>,
}

#[derive(Default)]
pub(crate) struct RunState {
    gate: Mutex<()>,
    active: Mutex<Option<ActiveRun>>,
    active_launch: Mutex<Option<u64>>,
    pending_launch: Mutex<Option<u64>>,
    progress_window_closing: Mutex<bool>,
    pub(crate) seq: AtomicU64,
    launch_seq: AtomicU64,
    commands_in_flight: AtomicU64,
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
        return Err(
            "Another run is already in progress — cancel it or wait for it to finish".into(),
        );
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
        if let Some(run) = st.active.lock().unwrap().as_ref() {
            run.ctl.request_cancel();
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
        return Err(
            "Another run is already in progress — cancel it or wait for it to finish".into(),
        );
    }
    let mut pending = st.pending_launch.lock().unwrap();
    match launch_id {
        Some(id) if *pending == Some(id) => *pending = None,
        Some(_) => return Err("This synchronization launch is no longer active".into()),
        None if pending.is_some() => {
            return Err("A synchronization is already preparing to start".into())
        }
        None => {}
    }
    let run_id = st.seq.fetch_add(1, Ordering::Relaxed) + 1;
    let ctl = RunCtl::new();
    *g = Some(ActiveRun {
        id: run_id,
        ctl: ctl.clone(),
    });
    *st.active_launch.lock().unwrap() = launch_id;
    Ok((run_id, ctl))
}

pub(crate) fn end_run(st: &RunState, run_id: u64) -> bool {
    let _gate = st.gate.lock().unwrap();
    let mut active = st.active.lock().unwrap();
    if active.as_ref().map(|run| run.id) != Some(run_id) {
        return false;
    }
    *active = None;
    *st.active_launch.lock().unwrap() = None;
    true
}

pub(crate) fn has_run_activity(st: &RunState) -> bool {
    let _gate = st.gate.lock().unwrap();
    st.active.lock().unwrap().is_some()
        || st.pending_launch.lock().unwrap().is_some()
        || st.commands_in_flight.load(Ordering::Acquire) > 0
}

/// Run a short registry mutation only after proving the run pipeline is idle, while holding the
/// same gate every compare/apply entry point must cross. This closes the check-then-write race: a
/// run command that arrives during the filesystem mutation waits, then reads the completed job.
pub(crate) fn with_run_idle<T>(
    st: &RunState,
    operation: &str,
    mutate: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _gate = st.gate.lock().unwrap();
    if st.active.lock().unwrap().is_some()
        || st.pending_launch.lock().unwrap().is_some()
        || st.commands_in_flight.load(Ordering::Acquire) > 0
    {
        return Err(format!(
            "{operation} is unavailable while Compare or Synchronize is preparing or running"
        ));
    }
    mutate()
}

pub(crate) fn begin_run_command(st: &RunState) {
    let _gate = st.gate.lock().unwrap();
    st.commands_in_flight.fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn finish_run_command(st: &RunState) {
    let _gate = st.gate.lock().unwrap();
    let previous = st.commands_in_flight.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(previous > 0, "run-command activity underflow");
}

pub(crate) fn request_cancel(st: &RunState, run_id: u64) -> Result<bool, String> {
    let active = st.active.lock().unwrap();
    let Some(run) = active.as_ref() else {
        return Ok(false);
    };
    if run.id != run_id {
        return Err(format!(
            "Run {run_id} is no longer active; run {} is active now",
            run.id
        ));
    }
    run.ctl.request_cancel();
    Ok(true)
}

pub(crate) fn set_paused(st: &RunState, run_id: u64, paused: bool) -> Result<bool, String> {
    let active = st.active.lock().unwrap();
    let Some(run) = active.as_ref() else {
        return Ok(false);
    };
    if run.id != run_id {
        return Err(format!(
            "Run {run_id} is no longer active; run {} is active now",
            run.id
        ));
    }
    run.ctl.set_paused(paused);
    Ok(true)
}

// Event bridge
/// 1:N: resolve a multi-target job into "the single-job view of the currently selected target"
/// (the engine's single pipeline is reused as-is). Return the normalized index with it so every
/// caller binds provenance to the same target when the optional argument is absent.
pub(crate) fn resolve_target(
    job: &job::Job,
    target_index: Option<usize>,
) -> Result<(usize, job::Job), String> {
    job.validate_multi_target()?;
    let list = job.target_list();
    let idx = target_index.unwrap_or(0);
    let t = list
        .get(idx)
        .ok_or_else(|| format!("target index {idx} is out of range ({} total)", list.len()))?;
    Ok((idx, job.for_target(t)))
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
    if syncdash::obs::progress::is_cancelled(&e) {
        "cancelled".into()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syncdash::model::plan::Side;

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
    fn selected_rows_are_reconstructed_from_the_plan_including_valid_reversals() {
        let plan = [
            op(Action::Copy, "safe/file.txt"),
            op(Action::Update, "other.txt"),
        ];
        let selected = [
            SelectedRowDto {
                index: 1,
                flipped: false,
            },
            SelectedRowDto {
                index: 0,
                flipped: true,
            },
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
        let plan = [
            op(Action::Copy, "safe/file.txt"),
            op(Action::Conflict, "conflict.txt"),
        ];

        let empty = resolve_selected_ops(&plan, &[]).unwrap_err();
        assert!(empty.contains("No executable rows"), "{empty}");

        let outside = resolve_selected_ops(
            &plan,
            &[SelectedRowDto {
                index: 2,
                flipped: false,
            }],
        )
        .unwrap_err();
        assert!(outside.contains("outside"), "{outside}");

        let duplicate = resolve_selected_ops(
            &plan,
            &[
                SelectedRowDto {
                    index: 0,
                    flipped: false,
                },
                SelectedRowDto {
                    index: 0,
                    flipped: true,
                },
            ],
        )
        .unwrap_err();
        assert!(duplicate.contains("more than once"), "{duplicate}");

        let report = resolve_selected_ops(
            &plan,
            &[SelectedRowDto {
                index: 1,
                flipped: false,
            }],
        )
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

        let (run_id, ctl) = begin_run_for_launch(&state, Some(launch)).unwrap();
        let active = state.active.lock().unwrap().as_ref().unwrap().clone();
        assert_eq!(active.id, run_id);
        assert!(Arc::ptr_eq(&active.ctl, &ctl));
        assert_eq!(close_progress_launch(&state), "active");
        assert!(ctl.cancelled());
        assert!(end_run(&state, run_id));
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

    #[test]
    fn delayed_controls_cannot_target_a_newer_run() {
        let state = RunState::default();
        let (first, _) = begin_run(&state).unwrap();
        assert!(end_run(&state, first));
        let (second, ctl) = begin_run(&state).unwrap();

        let cancel = request_cancel(&state, first).unwrap_err();
        assert!(cancel.contains(&format!("run {second}")), "{cancel}");
        let pause = set_paused(&state, first, true).unwrap_err();
        assert!(pause.contains(&format!("run {second}")), "{pause}");
        assert!(!ctl.cancelled());

        assert!(set_paused(&state, second, true).unwrap());
        assert!(request_cancel(&state, second).unwrap());
        assert!(ctl.cancelled());
        assert!(!end_run(&state, first));
        assert!(has_run_activity(&state));
        assert!(end_run(&state, second));
        assert!(!has_run_activity(&state));
    }

    #[test]
    fn command_activity_closes_the_pre_run_window_race() {
        let state = RunState::default();
        begin_run_command(&state);
        assert!(has_run_activity(&state));
        finish_run_command(&state);
        assert!(!has_run_activity(&state));
    }

    #[test]
    fn idle_mutations_refuse_every_run_command_state() {
        let state = RunState::default();
        begin_run_command(&state);
        let mut called = false;
        let error = with_run_idle(&state, "Saving jobs", || {
            called = true;
            Ok(())
        })
        .unwrap_err();
        assert!(!called);
        assert!(error.contains("preparing or running"), "{error}");
        finish_run_command(&state);

        assert_eq!(with_run_idle(&state, "Saving jobs", || Ok(42)).unwrap(), 42);
    }
}

// Commands
