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
    let Some(start) = detection_start(
        app,
        generation,
        &binding,
        local_roots,
        &commands,
        &shared,
        &execution,
    ) else {
        return;
    };
    polling::run_polling(
        app, generation, &binding, &commands, &shared, &execution, start,
    );
}

/// The per-host native-detection seam: exactly one arm compiles for any target, mirroring the
/// predicates on the lane modules. The macOS arm runs FSEvents detection, which may serve the
/// worker's whole generation (`None`) or hand over to polling with its cursor state preserved.
/// A host without a native lane answers with an immediate polling start; Windows USN journals
/// and Linux inotify are future arms that replace the stub rather than growing new cfg in
/// `run_worker`.
#[cfg(target_os = "macos")]
fn detection_start(
    app: &tauri::AppHandle,
    generation: u64,
    binding: &AutoScanBinding,
    local_roots: Option<(PathBuf, PathBuf)>,
    commands: &mpsc::Receiver<WorkerCommand>,
    shared: &Arc<Mutex<AutoScanShared>>,
    execution: &AutoScanExecutionServices,
) -> Option<polling::PollingStart> {
    let Some((source, target)) = local_roots else {
        return Some(polling::PollingStart {
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
        native_macos::NativeExit::Stopped => None,
        native_macos::NativeExit::PollingRequired {
            detail,
            next_ticket,
        } => Some(polling::PollingStart {
            detail,
            next_ticket,
            immediate: false,
        }),
    }
}

#[cfg(not(target_os = "macos"))]
fn detection_start(
    _app: &tauri::AppHandle,
    _generation: u64,
    _binding: &AutoScanBinding,
    local_roots: Option<(PathBuf, PathBuf)>,
    _commands: &mpsc::Receiver<WorkerCommand>,
    _shared: &Arc<Mutex<AutoScanShared>>,
    _execution: &AutoScanExecutionServices,
) -> Option<polling::PollingStart> {
    let detail = if local_roots.is_some() {
        "Native filesystem events are not available on this platform; polling while SyncDash is open"
    } else {
        "These roots do not expose a local event journal; polling while SyncDash is open"
    };
    Some(polling::PollingStart {
        detail: detail.into(),
        next_ticket: 1,
        immediate: true,
    })
}
