//! Active worker ownership, event transport, and execution-service wiring.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use tauri::Emitter;

use crate::contracts::compare::CompareScopeExecutionStatusDto;
use crate::features::autoscan::authority::AutoScanComparePermit;
use crate::features::compare::evidence::model::result::SuccessfulComparePublication;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::window::MAIN_WINDOW_LABEL;

use super::model::{AutoScanBinding, AutoScanStatusDto};
use super::state::AutoScanShared;

pub(super) fn allocate_unique_id(counter: &AtomicU64, identity: &str) -> Result<u64, String> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| format!("The {identity} ID space is exhausted — restart SyncDash"))
}

pub(super) enum WorkerCommand {
    VerificationPublished { ticket_id: u64 },
    VerificationTerminated { ticket_id: u64 },
    Stop,
}

pub(super) struct ActiveAutoScan {
    pub(super) binding: AutoScanBinding,
    pub(super) generation: u64,
    pub(super) commands: mpsc::Sender<WorkerCommand>,
    pub(super) shared: Arc<Mutex<AutoScanShared>>,
    pub(super) events: AutoScanEvents,
    pub(super) join: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub(super) enum AutoScanEvents {
    Application(tauri::AppHandle),
    #[cfg(test)]
    Suppressed,
}

impl AutoScanEvents {
    pub(super) fn emit_execution_transition(&self, status: Option<CompareScopeExecutionStatusDto>) {
        if let (Self::Application(app), Some(status)) = (self, status) {
            let _ = app.emit_to(MAIN_WINDOW_LABEL, "compare-execution-status", status);
        }
    }

    pub(super) fn emit_status(&self, status: AutoScanStatusDto) {
        match self {
            Self::Application(app) => {
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "autoscan-status", status);
            }
            #[cfg(test)]
            Self::Suppressed => {}
        }
    }
}

#[derive(Clone)]
pub(super) struct AutoScanExecutionServices {
    pub(super) results: Arc<CompareResultRepository>,
    pub(super) authorizations: Arc<OperationAuthorizationStore>,
}

pub(super) enum AutoScanTerminalAuthority<'a> {
    TriggerRequest,
    UnlaunchedTrigger,
    ComparePermit(&'a AutoScanComparePermit),
}

pub(crate) struct AutoScanComparePublication {
    pub(crate) publication: SuccessfulComparePublication,
    pub(crate) autoscan_status: AutoScanStatusDto,
}
