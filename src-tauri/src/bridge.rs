//! The event bridge: engine `ProgressEvent`s out to the webview.
//!
//! `TauriSink` throttles and forwards them as `run-progress`, carrying a run_id so the frontend
//! can drop late events from a cancelled run, and a purpose so the sub-window takes only apply
//! while the main panel takes only compare.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::Emitter;

use syncdash::model::event::{Phase, ProgressEvent};
use syncdash::obs::progress::{ProgressSink, RunCtl, RunCtx};

use crate::window_role::{MAIN_WINDOW_LABEL, PROGRESS_WINDOW_LABEL};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunEventAudience {
    Compare,
    Apply,
}

impl RunEventAudience {
    const fn purpose(self) -> &'static str {
        match self {
            Self::Compare => "compare",
            Self::Apply => "apply",
        }
    }

    const fn window_label(self) -> &'static str {
        match self {
            Self::Compare => MAIN_WINDOW_LABEL,
            Self::Apply => PROGRESS_WINDOW_LABEL,
        }
    }
}

/// The run-progress payload sent to the frontend: run_id lets the frontend drop late events from a cancelled run;
/// purpose separates compare from apply — the sub-window only accepts apply (otherwise the automatic re-check compare
/// after a sync would hijack the open result window into a forever-spinning "comparing"), the main panel only accepts compare.
#[derive(Serialize, Clone, Debug)]
pub(crate) struct RunEvent {
    pub(crate) sequence: u64,
    pub(crate) run_id: u64,
    pub(crate) purpose: &'static str,
    #[serde(flatten)]
    pub(crate) ev: ProgressEvent,
}

const MAX_REPLAY_DIAGNOSTICS: usize = 256;

#[derive(Default)]
struct RunEventStore {
    run_id: Option<u64>,
    purpose: Option<&'static str>,
    next_sequence: u64,
    events: VecDeque<RunEvent>,
}

#[derive(Default)]
pub(crate) struct RunEventRepository(Mutex<RunEventStore>);

impl RunEventRepository {
    fn record(&self, run_id: u64, purpose: &'static str, ev: ProgressEvent) -> RunEvent {
        let mut store = self.0.lock().unwrap();
        if store.run_id != Some(run_id) {
            store.run_id = Some(run_id);
            store.purpose = Some(purpose);
            store.events.clear();
        }
        store.next_sequence = store.next_sequence.saturating_add(1);
        let event = RunEvent {
            sequence: store.next_sequence,
            run_id,
            purpose,
            ev,
        };
        compact_for(&mut store.events, &event.ev);
        if replayable(&event.ev) {
            store.events.push_back(event.clone());
            trim_diagnostics(&mut store.events);
        }
        event
    }

    pub(crate) fn replay(&self, purpose: &str, after_sequence: u64) -> Vec<RunEvent> {
        let store = self.0.lock().unwrap();
        if store.purpose != Some(purpose) {
            return Vec::new();
        }
        store
            .events
            .iter()
            .filter(|event| event.sequence > after_sequence)
            .cloned()
            .collect()
    }
}

fn replayable(ev: &ProgressEvent) -> bool {
    !matches!(
        ev,
        ProgressEvent::ItemResult { .. }
            | ProgressEvent::Log {
                level: syncdash::model::event::LogLevel::Info,
                ..
            }
    )
}

fn compact_for(events: &mut VecDeque<RunEvent>, next: &ProgressEvent) {
    match next {
        ProgressEvent::Progress { phase, .. } => {
            events.retain(|event| {
                !matches!(&event.ev, ProgressEvent::Progress { phase: old, .. } if old == phase)
            });
        }
        ProgressEvent::Totals { phase, .. } => {
            events.retain(|event| {
                !matches!(
                    &event.ev,
                    ProgressEvent::Progress { phase: old, .. }
                        | ProgressEvent::Totals { phase: old, .. }
                        if old == phase
                )
            });
        }
        ProgressEvent::Paused { .. } | ProgressEvent::Resumed { .. } => {
            events.retain(|event| {
                !matches!(
                    &event.ev,
                    ProgressEvent::Paused { .. } | ProgressEvent::Resumed { .. }
                )
            });
        }
        _ => {}
    }
}

fn diagnostic(ev: &ProgressEvent) -> bool {
    matches!(
        ev,
        ProgressEvent::Error { .. }
            | ProgressEvent::Log {
                level: syncdash::model::event::LogLevel::Warn
                    | syncdash::model::event::LogLevel::Error,
                ..
            }
    )
}

fn trim_diagnostics(events: &mut VecDeque<RunEvent>) {
    while events.iter().filter(|event| diagnostic(&event.ev)).count() > MAX_REPLAY_DIAGNOSTICS {
        let Some(index) = events.iter().position(|event| diagnostic(&event.ev)) else {
            break;
        };
        events.remove(index);
    }
}

fn phase_slot(p: Phase) -> usize {
    match p {
        Phase::ScanSource => 0,
        Phase::ScanTarget => 1,
        Phase::Compare => 2,
        Phase::Apply => 3,
        Phase::Pack => 4,
        Phase::Ship => 5,
        Phase::Verify => 6,
        Phase::Refresh => 7,
        Phase::Archive => 8,
    }
}

struct ProgressThrottle {
    last_ms: [AtomicU64; 9],
}

impl Default for ProgressThrottle {
    fn default() -> Self {
        Self {
            last_ms: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl ProgressThrottle {
    fn allows(&self, phase: Phase, ts_ms: u64) -> bool {
        self.last_ms[phase_slot(phase)]
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
                (ts_ms.saturating_sub(last) >= 100).then_some(ts_ms)
            })
            .is_ok()
    }
}

/// ProgressSink → Tauri events. Progress events are throttled to ≥100ms apiece (= the FFS chart sampling rate),
/// PhaseStart/Totals/PhaseEnd/Error/Paused/Resumed/Summary pass straight through.
pub(crate) struct TauriSink {
    app: tauri::AppHandle,
    events: Arc<RunEventRepository>,
    run_id: u64,
    audience: RunEventAudience,
    throttle: ProgressThrottle,
}

impl ProgressSink for TauriSink {
    fn emit(&self, ev: ProgressEvent) {
        if let ProgressEvent::Progress { phase, ts_ms, .. } = &ev {
            if !self.throttle.allows(*phase, *ts_ms) {
                return;
            }
        }
        let event = self.events.record(self.run_id, self.audience.purpose(), ev);
        let _ = self
            .app
            .emit_to(self.audience.window_label(), "run-progress", event);
    }
}

pub(crate) fn make_ctx(
    app: &tauri::AppHandle,
    events: Arc<RunEventRepository>,
    run_id: u64,
    ctl: Arc<RunCtl>,
    audience: RunEventAudience,
) -> RunCtx {
    RunCtx::new(
        ctl,
        Arc::new(TauriSink {
            app: app.clone(),
            events,
            run_id,
            audience,
            throttle: ProgressThrottle::default(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_phases_are_throttled_independently() {
        let throttle = ProgressThrottle::default();

        assert!(throttle.allows(Phase::ScanSource, 1_000));
        assert!(throttle.allows(Phase::ScanTarget, 1_001));
        assert!(!throttle.allows(Phase::ScanSource, 1_050));
        assert!(throttle.allows(Phase::ScanSource, 1_100));
    }

    #[test]
    fn replay_is_run_scoped_and_coalesces_progress_without_losing_boundaries() {
        let repository = RunEventRepository::default();
        repository.record(
            1,
            "compare",
            ProgressEvent::PhaseStart {
                phase: Phase::ScanSource,
                ts_ms: 1,
                label: None,
                items_total: 0,
                bytes_total: 0,
            },
        );
        for done in [1, 2] {
            repository.record(
                1,
                "compare",
                ProgressEvent::Progress {
                    phase: Phase::ScanSource,
                    ts_ms: done,
                    items_done: done,
                    items_total: 2,
                    bytes_done: done,
                    bytes_total: 2,
                    current_path: format!("{done}.txt"),
                },
            );
        }

        let replay = repository.replay("compare", 0);
        assert_eq!(replay.len(), 2);
        assert!(matches!(&replay[0].ev, ProgressEvent::PhaseStart { .. }));
        assert!(matches!(
            &replay[1].ev,
            ProgressEvent::Progress { items_done: 2, .. }
        ));
        assert!(repository.replay("apply", 0).is_empty());

        repository.record(2, "apply", ProgressEvent::Paused { ts_ms: 3 });
        assert!(repository.replay("compare", 0).is_empty());
        let next_run = repository.replay("apply", 0);
        assert_eq!(next_run.len(), 1);
        assert!(next_run[0].sequence > replay[1].sequence);
        assert!(repository.replay("apply", next_run[0].sequence).is_empty());
    }
}
