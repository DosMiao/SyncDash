//! The event bridge: engine `ProgressEvent`s out to the webview.
//!
//! `TauriSink` throttles and forwards them as `run-progress`, carrying a run_id so the frontend
//! can drop late events from a cancelled run, and a purpose so the sub-window takes only apply
//! while the main panel takes only compare.
//!
//! `legacy_shim` synthesizes the older flat `progress` event from PhaseStart/Progress. It was
//! meant to die when the M2 frontend landed; the progress sub-window migrated, the main window's
//! status line did not, so it is still load-bearing for `App.tsx`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::Emitter;

use syncdash::model::event::{Phase, ProgressEvent};
use syncdash::obs::progress::{ProgressSink, RunCtl, RunCtx};

use crate::dto::LegacyProgress;

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

pub(crate) fn legacy_phase(p: Phase) -> &'static str {
    match p {
        Phase::ScanSource => "scan-source",
        Phase::ScanTarget => "scan-target",
        Phase::Compare => "comparing",
        Phase::Apply => "applying",
        Phase::Pack => "packing",
        Phase::Ship => "shipping",
        Phase::Verify => "verifying",
        Phase::Refresh => "refreshing",
    }
}

pub(crate) fn legacy_shim(app: &tauri::AppHandle, ev: &ProgressEvent) {
    match ev {
        ProgressEvent::PhaseStart { phase, label, .. } => {
            let _ = app.emit(
                "progress",
                LegacyProgress {
                    phase: legacy_phase(*phase).into(),
                    detail: label.clone().unwrap_or_default(),
                    pct: -1,
                    rate: 0.0,
                },
            );
        }
        ProgressEvent::Progress { phase, bytes_done, bytes_total, items_done, items_total, .. } => {
            let pct = if *bytes_total > 0 {
                (bytes_done * 100 / bytes_total) as i32
            } else if *items_total > 0 {
                (items_done * 100 / items_total) as i32
            } else {
                -1
            };
            let _ = app.emit(
                "progress",
                LegacyProgress {
                    phase: legacy_phase(*phase).into(),
                    detail: format!(
                        "{} / {}",
                        syncdash::foundation::fmt::human_bytes(*bytes_done),
                        syncdash::foundation::fmt::human_bytes(*bytes_total)
                    ),
                    pct,
                    rate: 0.0,
                },
            );
        }
        _ => {}
    }
}

/// ProgressSink → Tauri events. Progress events are throttled to ≥100ms apiece (= the FFS chart sampling rate),
/// PhaseStart/Totals/Error/Paused/Resumed/Summary pass straight through.
pub(crate) struct TauriSink {
    app: tauri::AppHandle,
    run_id: u64,
    purpose: &'static str,
    last_progress_ms: AtomicU64,
}

impl ProgressSink for TauriSink {
    fn emit(&self, ev: ProgressEvent) {
        if let ProgressEvent::Progress { ts_ms, .. } = &ev {
            let last = self.last_progress_ms.load(Ordering::Relaxed);
            if ts_ms.saturating_sub(last) < 100 {
                return;
            }
            self.last_progress_ms.store(*ts_ms, Ordering::Relaxed);
        }
        legacy_shim(&self.app, &ev);
        let _ = self.app.emit("run-progress", RunEvent { run_id: self.run_id, purpose: self.purpose, ev });
    }
}

pub(crate) fn make_ctx(app: &tauri::AppHandle, run_id: u64, ctl: Arc<RunCtl>, purpose: &'static str) -> RunCtx {
    RunCtx::new(
        ctl,
        Arc::new(TauriSink { app: app.clone(), run_id, purpose, last_progress_ms: AtomicU64::new(0) }),
    )
}
