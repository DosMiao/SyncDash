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
    #[cfg(target_os = "macos")]
    if let Some((source, target)) = local_roots {
        match native_macos::run_native_macos(
            app,
            generation,
            &binding,
            (&source, &target),
            &commands,
            &shared,
            &execution,
        ) {
            native_macos::NativeExit::Stopped => return,
            native_macos::NativeExit::PollingRequired {
                detail,
                next_ticket,
            } => {
                polling::run_polling(
                    app,
                    generation,
                    &binding,
                    &commands,
                    &shared,
                    &execution,
                    polling::PollingStart {
                        detail,
                        next_ticket,
                        immediate: false,
                    },
                );
                return;
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    let polling_detail = if local_roots.is_some() {
        "Native filesystem events are not available on this platform; polling while SyncDash is open"
    } else {
        "These roots do not expose a local event journal; polling while SyncDash is open"
    };
    #[cfg(target_os = "macos")]
    let polling_detail =
        "These roots do not expose a local FSEvents journal; polling while SyncDash is open";
    polling::run_polling(
        app,
        generation,
        &binding,
        &commands,
        &shared,
        &execution,
        polling::PollingStart {
            detail: polling_detail.into(),
            next_ticket: 1,
            immediate: true,
        },
    );
}
