//! AutoScan worker routing across native journals and polling detection.

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use super::model::AutoScanBinding;
use super::runtime::{AutoScanExecutionServices, WorkerCommand};
use super::state::AutoScanShared;

pub(in crate::features::autoscan) mod binding;
pub(crate) mod configuration;
#[cfg(target_os = "macos")]
mod native_macos;
pub(in crate::features::autoscan) mod observation;
mod polling;

pub(super) fn run_worker(
    app: &tauri::AppHandle,
    generation: u64,
    binding: AutoScanBinding,
    local_roots: Option<(PathBuf, PathBuf)>,
    commands: mpsc::Receiver<WorkerCommand>,
    shared: Arc<Mutex<AutoScanShared>>,
    execution: AutoScanExecutionServices,
) {
    match detect(
        app,
        generation,
        &binding,
        local_roots,
        &commands,
        &shared,
        &execution,
    ) {
        DetectionOutcome::Complete => {}
        DetectionOutcome::HandOffToPolling(start) => polling::run_polling(
            app, generation, &binding, &commands, &shared, &execution, start,
        ),
    }
}

/// How a worker generation leaves the detection stage. Naming both exits keeps the one that ends
/// the worker from hiding inside an `Option`.
///
/// The pair is the seam's contract and does not vary by host; which arm can produce which does.
/// The expectation lapses — loudly — on the day this host grows a native lane.
#[cfg_attr(
    not(target_os = "macos"),
    expect(dead_code, reason = "no native lane can own a generation on this host")
)]
enum DetectionOutcome {
    /// A native lane owned the whole generation and has stopped; the worker is done.
    Complete,
    /// Polling owns the rest of this generation, resuming from this state.
    HandOffToPolling(polling::PollingStart),
}

/// The per-host detection seam: exactly one arm compiles for any target, mirroring the predicates
/// on the lane modules. Windows USN journals and Linux inotify are future arms that replace the
/// no-native-lane one rather than growing new cfg in `run_worker`.
#[cfg(target_os = "macos")]
fn detect(
    app: &tauri::AppHandle,
    generation: u64,
    binding: &AutoScanBinding,
    local_roots: Option<(PathBuf, PathBuf)>,
    commands: &mpsc::Receiver<WorkerCommand>,
    shared: &Arc<Mutex<AutoScanShared>>,
    execution: &AutoScanExecutionServices,
) -> DetectionOutcome {
    let Some((source, target)) = local_roots else {
        return DetectionOutcome::HandOffToPolling(polling::PollingStart {
            detail:
                "These roots do not expose a local FSEvents journal; polling while SyncDash is open"
                    .into(),
            next_ticket: 1,
            immediate: true,
        });
    };
    match native_macos::run_native_macos(
        app,
        generation,
        binding,
        (&source, &target),
        commands,
        shared,
        execution,
    ) {
        native_macos::NativeExit::Stopped => DetectionOutcome::Complete,
        native_macos::NativeExit::PollingRequired {
            detail,
            next_ticket,
        } => DetectionOutcome::HandOffToPolling(polling::PollingStart {
            detail,
            next_ticket,
            immediate: false,
        }),
    }
}

#[cfg(not(target_os = "macos"))]
fn detect(
    _app: &tauri::AppHandle,
    _generation: u64,
    _binding: &AutoScanBinding,
    local_roots: Option<(PathBuf, PathBuf)>,
    _commands: &mpsc::Receiver<WorkerCommand>,
    _shared: &Arc<Mutex<AutoScanShared>>,
    _execution: &AutoScanExecutionServices,
) -> DetectionOutcome {
    let detail = if local_roots.is_some() {
        "Native filesystem events are not available on this platform; polling while SyncDash is open"
    } else {
        "These roots do not expose a local event journal; polling while SyncDash is open"
    };
    DetectionOutcome::HandOffToPolling(polling::PollingStart {
        detail: detail.into(),
        next_ticket: 1,
        immediate: true,
    })
}
