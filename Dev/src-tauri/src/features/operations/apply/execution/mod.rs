//! Authorized Apply launch and run execution.

mod launch;

use std::sync::Arc;

use syncdash::pipeline::guard::caps::CapabilityConsent;
use syncdash::run;
use tauri::Emitter;

use crate::contracts::operations::{ApplyDto, PostRunPowerActionReadyDto};
use crate::features::autoscan::controller::AutoScanController;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::apply::ApplyAuthorization;
use crate::features::operations::authorization::store::OperationAuthorizationStore;
use crate::features::operations::events::model::RunEventAudience;
use crate::features::operations::events::repository::RunEventRepository;
use crate::features::operations::events::sink::make_ctx;
use crate::features::operations::lifecycle::coordinator::RunLifecycle;
use crate::window::PROGRESS_WINDOW_LABEL;

use self::launch::{revalidate_retained_before_apply, ApplyLaunch};
use super::super::execution::error::format_run_io_error;
use super::super::execution::guard::{AppliedResultGuard, RunRejected};
use super::super::projection::capability_blockers;
use super::preparation::{
    apply_facts, autoscan_health_refusals, build_apply_review, prepare_apply,
};

#[allow(clippy::too_many_arguments)] // The use case coordinates independent state authorities.
pub(crate) async fn apply_job(
    app: tauri::AppHandle,
    lifecycle: Arc<RunLifecycle>,
    results: Arc<CompareResultRepository>,
    events: Arc<RunEventRepository>,
    authorizations: Arc<OperationAuthorizationStore>,
    autoscan: Arc<AutoScanController>,
    authorization_token: String,
    launch_id: Option<u64>,
) -> Result<ApplyDto, String> {
    let command = lifecycle.command_lease()?;
    let rejection_lifecycle = lifecycle.clone();
    let power_action_lifecycle = lifecycle.clone();
    let requested_launch = launch_id;
    let run_app = app.clone();
    let joined =
        tauri::async_runtime::spawn_blocking(move || -> Result<ApplyDto, (String, bool)> {
            let authorization = authorizations
                .consume_apply_authorization(&authorization_token)
                .map_err(|error| (error, false))?;
            let apply_launch = ApplyLaunch::bind(authorization.kind(), launch_id)
                .map_err(|error| (error, false))?;
            let reviewed = authorization.review().clone();
            let health_warning_acknowledged = authorization.health_warning_acknowledged();
            let auto_apply_ticket = match &authorization {
                ApplyAuthorization::Interactive(_) => None,
                ApplyAuthorization::AutoScan(authorization) => Some(authorization.ticket().clone()),
            };
            let prepared = prepare_apply(
                &results,
                reviewed.compare_identity(),
                reviewed.reviewed_row_decisions().to_vec(),
            )
            .map_err(|error| (error, false))?;
            let facts = apply_facts(&prepared).map_err(|error| (error, false))?;
            let current_review =
                build_apply_review(&prepared, &facts).map_err(|error| (error, false))?;
            reviewed
                .verify_current(&current_review)
                .map_err(|error| (error, false))?;
            let blockers = capability_blockers(&facts.capabilities);
            if !blockers.is_empty() {
                return Err((blockers.join("\n"), false));
            }
            if matches!(authorization, ApplyAuthorization::AutoScan(_)) {
                let health_refusals = autoscan_health_refusals(&facts);
                if !health_refusals.is_empty() {
                    return Err((
                        format!(
                            "AutoScan Apply requires a completely clean health review:\n{}",
                            health_refusals.join("\n")
                        ),
                        false,
                    ));
                }
            }
            let verdict = if health_warning_acknowledged {
                &facts.acknowledged
            } else {
                &facts.unacknowledged
            };
            if !verdict.ok() {
                return Err((verdict.blockers.join("\n"), false));
            }
            let consent = CapabilityConsent::ExactDigest(
                current_review.capability_review_digest().to_string(),
            );
            let reserve =
                || revalidate_retained_before_apply(&results, &prepared, &command, apply_launch);
            let active_run = match auto_apply_ticket.as_ref() {
                Some(ticket) => autoscan.consume_authorized_with(ticket, reserve),
                None => reserve(),
            }
            .map_err(|error| (error, false))?;
            let run_id = active_run.run_id();
            let ctl = active_run.control();
            let mut applied_result = AppliedResultGuard::new(
                run_app.clone(),
                results.clone(),
                &prepared.target.registered_job.job_id,
                &prepared.target.config_revision,
            );
            let ctx = make_ctx(&run_app, events, run_id, ctl, RunEventAudience::Apply);
            let t0 = std::time::Instant::now();
            let recorder = syncdash::run::history::Recorder::start(
                syncdash::run::history::RunSubject::registered(
                    &prepared.target.job_name,
                    &prepared.target.registered_job.job_id,
                    prepared.target.target_index,
                ),
                run::apply_run_kind(&prepared.target.target_job),
                &ctx,
                &prepared.reviewed_operations,
            );
            let execution = run::apply_with_capability_consent_classified(
                &prepared.target.job_name,
                &prepared.target.target_job,
                &prepared.plan,
                &prepared.reviewed_operations,
                None,
                false,
                health_warning_acknowledged,
                &consent,
                &recorder.ctx,
            );
            let writes_started = execution.writes_started();
            if !writes_started {
                applied_result.retain_for_safe_rejection();
            }
            let outcome = match execution.into_result() {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Err((format_run_io_error(error), true));
                }
            };
            let _ = recorder.finish(&outcome, t0.elapsed().as_millis() as u64);
            if matches!(apply_launch, ApplyLaunch::Interactive { .. })
                && writes_started
                && !outcome.cancelled
                && outcome.errors == 0
            {
                match power_action_lifecycle.issue_post_run_power_action_grant(run_id) {
                    Ok(()) => match run_app.emit_to(
                        PROGRESS_WINDOW_LABEL,
                        "post-run-power-action-ready",
                        PostRunPowerActionReadyDto { run_id },
                    ) {
                        Ok(()) => {}
                        Err(error) => {
                            power_action_lifecycle.revoke_post_run_power_action_grant(run_id);
                            syncdash::log_warn!(
                                "desktop",
                                "Apply run {run_id} finished safely, but its power-action availability could not be delivered: {error}"
                            );
                        }
                    },
                    Err(error) => syncdash::log_error!(
                        "desktop",
                        "Apply run {run_id} finished safely, but its power-action grant could not be issued: {error}"
                    ),
                }
            }
            Ok(ApplyDto {
                done: outcome.done,
                skipped: outcome.skipped,
                errors: outcome.errors,
                bytes_copied: outcome.bytes_copied,
                cancelled: outcome.cancelled,
            })
        })
        .await;
    let result = match joined {
        Ok(result) => result,
        Err(error) => {
            let message = error.to_string();
            if let Some(launch_id) = requested_launch {
                rejection_lifecycle.cancel_progress_launch(launch_id);
                let _ = app.emit_to(
                    PROGRESS_WINDOW_LABEL,
                    "run-rejected",
                    RunRejected {
                        launch_id,
                        message: message.clone(),
                    },
                );
            }
            return Err(message);
        }
    };
    match result {
        Ok(outcome) => Ok(outcome),
        Err((message, began)) => {
            if !began {
                if let Some(launch_id) = requested_launch {
                    if rejection_lifecycle.cancel_progress_launch(launch_id) {
                        let _ = app.emit_to(
                            PROGRESS_WINDOW_LABEL,
                            "run-rejected",
                            RunRejected {
                                launch_id,
                                message: message.clone(),
                            },
                        );
                    }
                }
            }
            Err(message)
        }
    }
}
