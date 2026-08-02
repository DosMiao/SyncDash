use std::sync::Arc;

use tauri::Emitter;

use crate::features::autoscan::authority::AutoScanComparePermit;
use crate::features::autoscan::controller::AutoScanController;
use crate::features::autoscan::model::AutoScanVerificationTerminal;
use crate::features::compare::evidence::model::scope::CompareScope;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::window::MAIN_WINDOW_LABEL;

#[derive(Clone, serde::Serialize)]
pub(in crate::features::operations) struct RunRejected {
    pub(in crate::features::operations) launch_id: u64,
    pub(in crate::features::operations) message: String,
}

pub(in crate::features::operations) struct AutoScanCompareTerminalGuard {
    controller: Arc<AutoScanController>,
    permit: Option<AutoScanComparePermit>,
}

impl AutoScanCompareTerminalGuard {
    pub(in crate::features::operations) fn new(
        controller: Arc<AutoScanController>,
        permit: Option<AutoScanComparePermit>,
    ) -> Self {
        Self { controller, permit }
    }

    pub(in crate::features::operations) fn disarm(&mut self) {
        self.permit = None;
    }
}

impl Drop for AutoScanCompareTerminalGuard {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            let _ = self.controller.terminalize_permitted_verification(
                &permit,
                AutoScanVerificationTerminal::Failed(
                    "The Compare task ended without publishing or reporting a terminal outcome"
                        .into(),
                ),
            );
        }
    }
}

pub(in crate::features::operations) fn emit_compare_execution_status(
    app: &tauri::AppHandle,
    results: &CompareResultRepository,
    scope: &CompareScope,
) {
    let _ = app.emit_to(
        MAIN_WINDOW_LABEL,
        "compare-execution-status",
        results.execution_status(scope),
    );
}

pub(in crate::features::operations) struct AppliedResultGuard {
    app: tauri::AppHandle,
    results: Arc<CompareResultRepository>,
    job_id: String,
    config_revision: String,
    invalidate_on_drop: bool,
}

impl AppliedResultGuard {
    pub(in crate::features::operations) fn new(
        app: tauri::AppHandle,
        results: Arc<CompareResultRepository>,
        job_id: &str,
        config_revision: &str,
    ) -> Self {
        Self {
            app,
            results,
            job_id: job_id.to_string(),
            config_revision: config_revision.to_string(),
            invalidate_on_drop: true,
        }
    }

    pub(in crate::features::operations) fn retain_for_safe_rejection(&mut self) {
        self.invalidate_on_drop = false;
    }
}

impl Drop for AppliedResultGuard {
    fn drop(&mut self) {
        if self.invalidate_on_drop {
            for status in self.results.expire_revision(
                &self.job_id,
                &self.config_revision,
                crate::contracts::compare::CompareExecutionExpiryReasonDto::WriteStarted,
            ) {
                let _ = self
                    .app
                    .emit_to(MAIN_WINDOW_LABEL, "compare-execution-status", status);
            }
        }
    }
}
