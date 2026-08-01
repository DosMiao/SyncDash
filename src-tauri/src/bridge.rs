//! The event bridge: engine `ProgressEvent`s out to the webview.
//!
//! `TauriSink` throttles and forwards them as `run-progress`, carrying a run_id so the frontend
//! can drop late events from a cancelled run, and a purpose so the sub-window takes only apply
//! while the main panel takes only compare.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::Emitter;

use syncdash::model::event::{Phase, ProgressEvent};
use syncdash::obs::progress::{ProgressSink, RunCtl, RunCtx};

/// The run-progress payload sent to the frontend: run_id lets the frontend drop late events from a cancelled run;
/// purpose separates compare from apply — the sub-window only accepts apply (otherwise the automatic re-check compare
/// after a sync would hijack the open result window into a forever-spinning "comparing"), the main panel only accepts compare.
#[derive(Serialize, Clone)]
pub(crate) struct RunEvent {
    run_id: u64,
    purpose: &'static str,
    #[serde(flatten)]
    ev: ProgressEvent,
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
        Self { last_ms: std::array::from_fn(|_| AtomicU64::new(0)) }
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
    run_id: u64,
    purpose: &'static str,
    throttle: ProgressThrottle,
}

impl ProgressSink for TauriSink {
    fn emit(&self, ev: ProgressEvent) {
        if let ProgressEvent::Progress { phase, ts_ms, .. } = &ev {
            if !self.throttle.allows(*phase, *ts_ms) {
                return;
            }
        }
        let _ = self.app.emit("run-progress", RunEvent { run_id: self.run_id, purpose: self.purpose, ev });
    }
}

pub(crate) fn make_ctx(app: &tauri::AppHandle, run_id: u64, ctl: Arc<RunCtl>, purpose: &'static str) -> RunCtx {
    RunCtx::new(
        ctl,
        Arc::new(TauriSink { app: app.clone(), run_id, purpose, throttle: ProgressThrottle::default() }),
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
}
