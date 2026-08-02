//! Authorized Compare launch, evidence publication, and terminalization.

mod publication;

use std::sync::Arc;

use syncdash::pipeline::guard::caps::CapabilityConsent;
use syncdash::run;
use tauri::Emitter;

use crate::contracts::compare::CompareWorkspaceSnapshotDto;
use crate::features::autoscan::controller::AutoScanController;
use crate::features::autoscan::model::AutoScanVerificationTerminal;
use crate::features::autoscan::runtime::AutoScanComparePublication;
use crate::features::compare::evidence::model::scope::CompareScope;
use crate::features::compare::evidence::model::verification::CompareVerificationTerminalOutcome;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::events::model::RunEventAudience;
use crate::features::operations::events::repository::RunEventRepository;
use crate::features::operations::events::sink::make_ctx;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;
use crate::features::operations::lifecycle::model::RunPurpose;
use crate::window::MAIN_WINDOW_LABEL;

use self::publication::prepare_successful_result;
use super::super::execution::error::{format_run_io_error, verification_terminal_from_io};
use super::super::execution::guard::{emit_compare_execution_status, AutoScanCompareTerminalGuard};
use super::super::projection::capability_blockers;
use super::super::target::{
    build_compare_authorization, load_bound_target, reload_prepared_target,
};

#[allow(clippy::too_many_arguments)] // The use case coordinates independent state authorities.
pub(crate) async fn compare_job(
    app: tauri::AppHandle,
    lifecycle: Arc<RunLifecycle>,
    results: Arc<CompareResultRepository>,
    events: Arc<RunEventRepository>,
    authorizations: Arc<OperationAuthorizationStore>,
    autoscan: Arc<AutoScanController>,
    authorization_token: String,
) -> Result<CompareWorkspaceSnapshotDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let authorization = authorizations.consume_compare_authorization(&authorization_token)?;
        let auto_scan_permit = authorization.auto_scan_permit().cloned();
        let mut auto_scan_terminal =
            AutoScanCompareTerminalGuard::new(autoscan.clone(), auto_scan_permit.clone());
        let terminalize_auto_scan = |terminal: AutoScanVerificationTerminal| {
            if let Some(permit) = auto_scan_permit.as_ref() {
                let _ = autoscan.terminalize_permitted_verification(permit, terminal);
            }
        };
        let command = match lifecycle.command_lease() {
            Ok(command) => command,
            Err(message) => {
                terminalize_auto_scan(AutoScanVerificationTerminal::Failed(message.clone()));
                return Err(message);
            }
        };
        let target = match load_bound_target(authorization.target()) {
            Ok(target) => target,
            Err(message) => {
                terminalize_auto_scan(AutoScanVerificationTerminal::Failed(message.clone()));
                return Err(message);
            }
        };
        let capabilities = match run::compare_capabilities(&target.target_job) {
            Ok(capabilities) => capabilities,
            Err(error) => {
                let terminal = verification_terminal_from_io(&error);
                let message = format_run_io_error(error);
                terminalize_auto_scan(terminal);
                return Err(message);
            }
        };
        let blockers = capability_blockers(&capabilities);
        if !blockers.is_empty() {
            let message = blockers.join("\n");
            terminalize_auto_scan(AutoScanVerificationTerminal::Failed(message.clone()));
            return Err(message);
        }
        let target = match reload_prepared_target(&target) {
            Ok(target) => target,
            Err(message) => {
                terminalize_auto_scan(AutoScanVerificationTerminal::Failed(message.clone()));
                return Err(message);
            }
        };
        let current = match build_compare_authorization(
            &target,
            &capabilities,
            authorization.origin().clone(),
        ) {
            Ok(current) => current,
            Err(message) => {
                terminalize_auto_scan(AutoScanVerificationTerminal::Failed(message.clone()));
                return Err(message);
            }
        };
        if let Err(message) = authorization.verify_current(&current) {
            terminalize_auto_scan(AutoScanVerificationTerminal::Failed(message.clone()));
            return Err(message);
        }
        let consent =
            CapabilityConsent::ExactDigest(current.capability_review_digest().to_string());

        let active_run = match command.start_run(RunPurpose::Compare) {
            Ok(active_run) => active_run,
            Err(message) => {
                terminalize_auto_scan(AutoScanVerificationTerminal::Failed(message.clone()));
                return Err(message);
            }
        };
        let run_id = active_run.run_id();
        let execution_scope = CompareScope::new(
            &target.registered_job.job_id,
            target.target_index,
            &target.config_revision,
        );
        authorizations.revoke_apply_authority(&execution_scope);
        let verification = match auto_scan_permit.as_ref() {
            Some(permit) => {
                if let Err(message) = autoscan.mark_compare_launched(permit, run_id) {
                    terminalize_auto_scan(AutoScanVerificationTerminal::Failed(message.clone()));
                    return Err(message);
                }
                permit.verification().clone()
            }
            None => results
                .begin_verification(execution_scope.clone(), Some(run_id))
                .map_err(|error| error.to_string())?,
        };
        emit_compare_execution_status(&app, &results, &execution_scope);
        let terminalize_failure = |terminal: AutoScanVerificationTerminal,
                                   repository_message: &str| {
            if auto_scan_permit.is_some() {
                terminalize_auto_scan(terminal);
            } else {
                let outcome = match terminal {
                    AutoScanVerificationTerminal::Failed(_) => {
                        CompareVerificationTerminalOutcome::Failed {
                            message: repository_message.to_string(),
                        }
                    }
                    AutoScanVerificationTerminal::Cancelled => {
                        CompareVerificationTerminalOutcome::Cancelled
                    }
                };
                if results.complete_verification_terminal(&verification, outcome) {
                    emit_compare_execution_status(&app, &results, &execution_scope);
                }
            }
        };
        let ctl = active_run.control();
        let ctx = make_ctx(&app, events, run_id, ctl, RunEventAudience::Compare);
        let outlet: Arc<dyn syncdash::obs::progress::ProgressSink> =
            match syncdash::obs::progress::current() {
                Some(previous) => Arc::new(syncdash::obs::logging::MultiSink::new(vec![
                    ctx.sink.clone(),
                    previous,
                ])),
                None => ctx.sink.clone(),
            };
        let _log_guard = syncdash::obs::progress::install(outlet);
        let t0 = std::time::Instant::now();
        let ts_ms = syncdash::foundation::time::now_ms() as i64;
        let result = run::compare_with_capability_consent(
            &target.job_name,
            &target.target_job,
            &ctx,
            &consent,
        );
        syncdash::run::history::compare_summary(
            syncdash::run::history::RunSubject::registered(
                &target.job_name,
                &target.registered_job.job_id,
                target.target_index,
            ),
            run::compare_run_kind(&target.target_job),
            ts_ms,
            result
                .as_ref()
                .map(|outcome| outcome.plan.ops.len() as u64)
                .unwrap_or(0),
            t0.elapsed().as_millis() as u64,
            result
                .as_ref()
                .err()
                .map(syncdash::obs::progress::is_cancelled)
                .unwrap_or(false),
        );
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let terminal = verification_terminal_from_io(&error);
                let message = format_run_io_error(error);
                terminalize_failure(terminal, &message);
                return Err(message);
            }
        };
        let retained_result = match prepare_successful_result(run_id, &target, outcome) {
            Ok(result) => result,
            Err(message) => {
                terminalize_failure(
                    AutoScanVerificationTerminal::Failed(message.clone()),
                    &message,
                );
                return Err(message);
            }
        };
        let publication = if let Some(permit) = auto_scan_permit.as_ref() {
            match autoscan.publish_successful_compare(permit, retained_result) {
                Ok(AutoScanComparePublication {
                    publication,
                    autoscan_status,
                }) => {
                    auto_scan_terminal.disarm();
                    debug_assert!(autoscan_status.pending_trigger.is_none());
                    publication
                }
                Err(message) => {
                    terminalize_failure(
                        AutoScanVerificationTerminal::Failed(message.clone()),
                        &message,
                    );
                    return Err(message);
                }
            }
        } else {
            match results.publish_successful_version(&verification, retained_result) {
                Ok(publication) => publication,
                Err(error) => {
                    let message = error.to_string();
                    terminalize_failure(
                        AutoScanVerificationTerminal::Failed(message.clone()),
                        &message,
                    );
                    return Err(message);
                }
            }
        };
        let _ = app.emit_to(
            MAIN_WINDOW_LABEL,
            "compare-execution-status",
            publication.workspace.execution_status.clone(),
        );
        Ok(publication.workspace)
    })
    .await
    .map_err(|error| error.to_string())?
}
