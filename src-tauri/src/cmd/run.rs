//! Running a job: compare, preflight, apply, and the cancel/pause controls over a live run.
//!
//! The transport choice — this process or an ssh peer — is `run`'s, not this module's. These call
//! `run::compare` / `run::preflight` / `run::apply` and let the router decide.

use std::sync::Arc;

use syncdash::model::plan::{Action, Op, Plan};
use syncdash::pipeline::compare;
use syncdash::pipeline::guard::caps::{CapReport, CapabilityConsent, CapabilityScope};
use syncdash::pipeline::guard::Verdict;
use syncdash::{job, run};
use tauri::Emitter;

use crate::autoscan::{AutoApplyTicket, AutoScanController};
use crate::bridge::{make_ctx, RunEvent, RunEventRepository};
use crate::compare_results::{
    validate_retained_compare, CompareResultRepository, CompareScope, SuccessfulCompareResult,
};
use crate::dto::{
    ApplyDto, ApplySessionGrantDecisionDto, AuthorizationDto, AutoScanCompareRequestDto,
    CapabilityIssueDto, CompareIdentity, CompareOwner, OperationApprovalDto, OperationReviewDto,
    PlanDto, SelectedRowDto,
};
use crate::operation_authorization::{
    health_review_digest, ApplyAuthorization, ApplyAuthorizationKind, ApplyReview,
    ApplySessionGrantDecision, CompareAuthorization, CompareOrigin, IssuedAuthorization,
    JobTargetRevision, OperationAuthorizationStore, ReviewApproval, ReviewChallenge,
};
use crate::operation_selection::{resolve_selected_operations, resolve_target};
use crate::run_lifecycle::{ActiveRunLease, RunCommandLease, RunLifecycle, RunPurpose};

use super::require_main_window;

#[derive(Clone, serde::Serialize)]
struct RunRejected {
    launch_id: u64,
    message: String,
}

fn format_run_io_error(error: std::io::Error) -> String {
    if syncdash::obs::progress::is_cancelled(&error) {
        "cancelled".into()
    } else {
        error.to_string()
    }
}

struct AppliedResultGuard {
    results: Arc<CompareResultRepository>,
    job_id: String,
    config_revision: String,
    invalidate_on_drop: bool,
}

impl AppliedResultGuard {
    fn new(results: Arc<CompareResultRepository>, job_id: &str, config_revision: &str) -> Self {
        Self {
            results,
            job_id: job_id.to_string(),
            config_revision: config_revision.to_string(),
            invalidate_on_drop: true,
        }
    }

    fn retain_for_safe_rejection(&mut self) {
        self.invalidate_on_drop = false;
    }
}

impl Drop for AppliedResultGuard {
    fn drop(&mut self) {
        if self.invalidate_on_drop {
            self.results
                .invalidate_revision(&self.job_id, &self.config_revision);
        }
    }
}

/// Request cooperative cancellation of the active run. Returns whether an active run existed.
#[tauri::command]
pub fn cancel_run(
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    run_id: u64,
) -> Result<bool, String> {
    lifecycle.request_cancel(run_id)
}

/// Pause/resume the active run (elapsed stops growing while paused, the RootLock heartbeat keeps beating)
#[tauri::command]
pub fn pause_run(
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    run_id: u64,
    paused: bool,
) -> Result<bool, String> {
    lifecycle.set_paused(run_id, paused)
}

#[tauri::command]
pub fn replay_run_events(
    events: tauri::State<'_, Arc<RunEventRepository>>,
    purpose: String,
    after_sequence: Option<u64>,
) -> Result<Vec<RunEvent>, String> {
    if !matches!(purpose.as_str(), "compare" | "apply") {
        return Err(format!("Unknown run purpose: {purpose}"));
    }
    Ok(events.replay(&purpose, after_sequence.unwrap_or(0)))
}

struct ResolvedJobTarget {
    job_name: String,
    registered_job: syncdash::job::Job,
    target_index: usize,
    target_job: syncdash::job::Job,
    config_revision: String,
}

struct PreparedApply {
    target: ResolvedJobTarget,
    owner: CompareOwner,
    plan: Plan,
    plan_digest: String,
    selected_rows: Vec<SelectedRowDto>,
    selected_operations: Vec<Op>,
}

struct RetainedApplyPlan {
    target: ResolvedJobTarget,
    owner: CompareOwner,
    plan: Plan,
    plan_digest: String,
}

struct ApplyFacts {
    unacknowledged: Verdict,
    acknowledged: Verdict,
    capabilities: CapReport,
}

fn load_review_target(
    expected_job_id: &str,
    target_index: Option<usize>,
) -> Result<ResolvedJobTarget, String> {
    let (job_name, registered_job) = job::load_by_id(expected_job_id).map_err(|error| {
        format!("The selected job was deleted or replaced — refresh it and review Compare again: {error}")
    })?;
    resolve_job_target(job_name, registered_job, target_index)
}

fn load_bound_target(target: &JobTargetRevision) -> Result<ResolvedJobTarget, String> {
    let (job_name, registered_job) = job::load_by_id(target.job_id()).map_err(|error| {
        format!("The authorized job was deleted or replaced — review the operation again: {error}")
    })?;
    let resolved = resolve_job_target(job_name, registered_job, Some(target.target_index()))?;
    if resolved.config_revision != target.config_revision() {
        return Err(format!(
            "Job '{}' changed after authorization — review the operation again",
            resolved.job_name
        ));
    }
    Ok(resolved)
}

fn resolve_job_target(
    job_name: String,
    registered_job: syncdash::job::Job,
    target_index: Option<usize>,
) -> Result<ResolvedJobTarget, String> {
    let config_revision = job::config_revision(&registered_job)
        .map_err(|error| format!("Job '{job_name}': {error}"))?;
    let (target_index, target_job) = resolve_target(&registered_job, target_index)?;
    Ok(ResolvedJobTarget {
        job_name,
        registered_job,
        target_index,
        target_job,
        config_revision,
    })
}

fn validate_resolved_target_unchanged(
    reviewed: &ResolvedJobTarget,
    current: &ResolvedJobTarget,
) -> Result<(), String> {
    if reviewed.registered_job.job_id != current.registered_job.job_id {
        return Err("The reviewed job identity changed — review the operation again".into());
    }
    if reviewed.config_revision != current.config_revision {
        return Err("The reviewed job configuration changed — review the operation again".into());
    }
    if reviewed.target_index != current.target_index {
        return Err("The reviewed target changed — review the operation again".into());
    }
    Ok(())
}

fn reload_prepared_target(reviewed: &ResolvedJobTarget) -> Result<ResolvedJobTarget, String> {
    let (job_name, registered_job) =
        job::load_by_id(&reviewed.registered_job.job_id).map_err(|error| {
            format!("The reviewed job was deleted or replaced — review again: {error}")
        })?;
    let current = resolve_job_target(job_name, registered_job, Some(reviewed.target_index))?;
    validate_resolved_target_unchanged(reviewed, &current)?;
    Ok(current)
}

fn build_compare_authorization(
    target: &ResolvedJobTarget,
    capabilities: &CapReport,
    origin: CompareOrigin,
) -> Result<CompareAuthorization, String> {
    CompareAuthorization::new(
        JobTargetRevision::new(
            target.registered_job.job_id.clone(),
            target.config_revision.clone(),
            target.target_index,
        )?,
        capabilities.consent_digest(CapabilityScope::CompareRead),
        origin,
    )
}

fn authorization_dto(issued: IssuedAuthorization) -> AuthorizationDto {
    AuthorizationDto {
        authorization_token: issued.authorization_token,
        expires_at_ms: issued.expires_at_ms,
    }
}

fn capability_dtos(capabilities: &CapReport) -> Vec<CapabilityIssueDto> {
    capabilities.items.iter().map(Into::into).collect()
}

fn capability_blockers(capabilities: &CapReport) -> Vec<String> {
    capabilities
        .blockers()
        .into_iter()
        .map(|item| item.render())
        .collect()
}

fn blocked_review(
    blockers: Vec<String>,
    warnings: Vec<String>,
    capabilities: &CapReport,
) -> OperationReviewDto {
    OperationReviewDto::Blocked {
        blockers,
        warnings,
        capabilities: capability_dtos(capabilities),
    }
}

#[tauri::command]
pub async fn review_compare(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    expected_job_id: String,
    target_index: Option<usize>,
    auto_scan_request: Option<AutoScanCompareRequestDto>,
) -> Result<OperationReviewDto, String> {
    require_main_window(&window)?;
    let _command = lifecycle.inner().command_lease()?;
    let authorizations = authorizations.inner().clone();
    let autoscan = autoscan.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let target = load_review_target(&expected_job_id, target_index)?;
        let capabilities = match run::compare_capabilities(&target.target_job) {
            Ok(capabilities) => capabilities,
            Err(error) => {
                return Ok(blocked_review(
                    vec![format_run_io_error(error)],
                    Vec::new(),
                    &CapReport::default(),
                ))
            }
        };
        let blockers = capability_blockers(&capabilities);
        if !blockers.is_empty() {
            return Ok(blocked_review(blockers, Vec::new(), &capabilities));
        }
        let origin = match auto_scan_request {
            Some(request) => CompareOrigin::AutoScan(
                autoscan.issue_compare_permit(request.generation, request.ticket_id)?,
            ),
            None => CompareOrigin::Interactive,
        };
        let authorization = build_compare_authorization(&target, &capabilities, origin)?;
        let requires_capability_ack = !capabilities.needs_ack().is_empty();
        if !requires_capability_ack || authorizations.has_compare_capability_grant(&authorization) {
            let issued = authorizations.issue_compare_authorization(authorization)?;
            return Ok(OperationReviewDto::DirectAuthorized {
                authorization: authorization_dto(issued),
                capabilities: capability_dtos(&capabilities),
            });
        }
        let challenge = authorizations.create_review_challenge(ReviewChallenge::Compare {
            authorization,
            requires_capability_ack: true,
        })?;
        Ok(OperationReviewDto::CompareConfirmationRequired {
            challenge_id: challenge.challenge_id,
            expires_at_ms: challenge.expires_at_ms,
            capabilities: capability_dtos(&capabilities),
            can_remember_for_session: true,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub fn approve_operation(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    challenge_id: String,
    approval: OperationApprovalDto,
) -> Result<AuthorizationDto, String> {
    require_main_window(&window)?;
    let _command = lifecycle.inner().command_lease()?;
    let approval = match approval {
        OperationApprovalDto::Compare {
            accept_capabilities,
            remember_for_session,
        } => ReviewApproval::Compare {
            accept_capabilities,
            remember_for_session,
        },
        OperationApprovalDto::InteractiveApply {
            acknowledge_health,
            accept_capabilities,
            session_grant,
        } => ReviewApproval::InteractiveApply {
            acknowledge_health,
            accept_capabilities,
            session_grant: match session_grant {
                ApplySessionGrantDecisionDto::None => ApplySessionGrantDecision::None,
                ApplySessionGrantDecisionDto::RememberCapabilities => {
                    ApplySessionGrantDecision::RememberCapabilities
                }
                ApplySessionGrantDecisionDto::AllowAutoApply => {
                    ApplySessionGrantDecision::AllowAutoApply
                }
            },
        },
    };
    let issued = authorizations.approve_review_challenge(&challenge_id, approval)?;
    Ok(authorization_dto(issued))
}

#[allow(clippy::too_many_arguments)] // Tauri injects state and exposes the rest as flat IPC fields.
#[tauri::command]
pub async fn compare_job(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    events: tauri::State<'_, Arc<RunEventRepository>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    authorization_token: String,
) -> Result<PlanDto, String> {
    require_main_window(&window)?;
    let lifecycle = lifecycle.inner().clone();
    let command = lifecycle.command_lease()?;
    let results = results.inner().clone();
    let events = events.inner().clone();
    let authorizations = authorizations.inner().clone();
    let autoscan = autoscan.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let authorization = authorizations.consume_compare_authorization(&authorization_token)?;
        let auto_scan_permit = authorization.auto_scan_permit().cloned();
        let target = load_bound_target(authorization.target())?;
        let capabilities =
            run::compare_capabilities(&target.target_job).map_err(format_run_io_error)?;
        let blockers = capability_blockers(&capabilities);
        if !blockers.is_empty() {
            return Err(blockers.join("\n"));
        }
        let target = reload_prepared_target(&target)?;
        let current =
            build_compare_authorization(&target, &capabilities, authorization.origin().clone())?;
        authorization.verify_current(&current)?;
        let consent =
            CapabilityConsent::ExactDigest(current.capability_review_digest().to_string());

        let active_run = command.start_run(RunPurpose::Compare)?;
        let run_id = active_run.run_id();
        let execution_scope = CompareScope::new(
            &target.registered_job.job_id,
            target.target_index,
            &target.config_revision,
        );
        let manual_verification = auto_scan_permit
            .is_none()
            .then(|| results.begin_verification(execution_scope.clone()));
        authorizations.revoke_apply_authority(&execution_scope);
        let manual_verification = manual_verification
            .transpose()
            .map_err(|error| error.to_string())?;
        let ctl = active_run.control();
        let ctx = make_ctx(&app, events, run_id, ctl, "compare");
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
        syncdash::obs::runlog::compare_summary(
            &target.job_name,
            &run::run_kind(&target.target_job, "compare"),
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
        let outcome = result.map_err(format_run_io_error)?;
        let plan_digest = outcome.plan.digest();
        let mut owner = CompareOwner {
            identity: CompareIdentity {
                compare_run_id: run_id,
                job_id: target.registered_job.job_id.clone(),
                target_index: target.target_index,
                config_revision: target.config_revision.clone(),
            },
            job_name: target.job_name.clone(),
        };
        let evidence = compare::evidence::evidence(
            &outcome.source,
            &outcome.target,
            &outcome.plan,
            &outcome.compare_options,
        );
        let metas = evidence
            .metas
            .into_iter()
            .zip(&outcome.plan.ops)
            .map(|(meta, op)| {
                if matches!(op.action, Action::Copy) && op.size.is_some() && op.mtime_ms.is_some() {
                    None
                } else {
                    Some(meta)
                }
            })
            .collect();
        let mut dto = PlanDto {
            owner: owner.clone(),
            header: outcome.plan.header,
            ops: outcome.plan.ops,
            metas,
            identical_count: evidence.identical_count,
            identical_bytes: evidence.identical_bytes,
        };

        let (current_name, current_job) =
            job::load_by_id(&owner.identity.job_id).map_err(|error| {
                format!(
                    "Job '{}' was deleted or replaced while Compare was running: {error}",
                    owner.job_name
                )
            })?;
        let current_revision = job::config_revision(&current_job)
            .map_err(|error| format!("Job '{}': {error}", owner.job_name))?;
        if current_revision != owner.identity.config_revision {
            return Err(format!(
                "Job '{}' changed while Compare was running — run Compare again",
                owner.job_name
            ));
        }
        owner.job_name = current_name;
        dto.owner = owner.clone();
        let retained_result = SuccessfulCompareResult::from_plan(
            plan_digest,
            dto.clone(),
            outcome.source,
            outcome.target,
            outcome.compare_options,
        );
        if let Some(permit) = auto_scan_permit {
            autoscan.publish_successful_compare(&permit, retained_result)?;
        } else {
            results
                .publish_successful_version(
                    manual_verification
                        .as_ref()
                        .expect("manual Compare must own a verification ticket"),
                    retained_result,
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(dto)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn prepare_apply(
    results: &CompareResultRepository,
    compare_identity: &CompareIdentity,
    selected_rows: Vec<SelectedRowDto>,
) -> Result<PreparedApply, String> {
    prepare_retained_apply(
        load_retained_apply(results, compare_identity)?,
        selected_rows,
    )
}

fn load_retained_apply(
    results: &CompareResultRepository,
    compare_identity: &CompareIdentity,
) -> Result<RetainedApplyPlan, String> {
    let (job_name, registered_job) =
        job::load_by_id(&compare_identity.job_id).map_err(|error| {
            format!("The Compare result's job was deleted or replaced — run Compare again: {error}")
        })?;
    let target = resolve_job_target(
        job_name,
        registered_job,
        Some(compare_identity.target_index),
    )?;
    if target.config_revision != compare_identity.config_revision {
        return Err(format!(
            "Job '{}' changed since this Compare — run Compare again",
            target.job_name
        ));
    }
    results.rebind_job_name(&target.registered_job.job_id, &target.job_name);
    let retained = results
        .get_fresh_exact(compare_identity)
        .map_err(|error| error.to_string())?;
    let requested_owner = CompareOwner {
        identity: compare_identity.clone(),
        job_name: target.job_name.clone(),
    };
    validate_retained_compare(
        Some(&retained),
        &requested_owner,
        &target.registered_job.job_id,
        &target.job_name,
        target.target_index,
        &target.config_revision,
        None,
    )?;
    let owner = retained.owner().clone();
    let plan = Plan {
        header: retained.plan_header().clone(),
        ops: retained.plan_operations().to_vec(),
    };
    let plan_digest = retained.plan_digest().to_string();
    if plan.digest() != plan_digest {
        return Err("The retained Compare plan changed — run Compare again".into());
    }
    Ok(RetainedApplyPlan {
        target,
        owner,
        plan,
        plan_digest,
    })
}

fn prepare_retained_apply(
    retained_plan: RetainedApplyPlan,
    selected_rows: Vec<SelectedRowDto>,
) -> Result<PreparedApply, String> {
    let selected_operations = resolve_selected_operations(&retained_plan.plan.ops, &selected_rows)?;
    Ok(PreparedApply {
        target: retained_plan.target,
        owner: retained_plan.owner,
        plan: retained_plan.plan,
        plan_digest: retained_plan.plan_digest,
        selected_rows,
        selected_operations,
    })
}

fn server_owned_selection(ops: &[Op]) -> Result<Vec<SelectedRowDto>, String> {
    let selected: Vec<SelectedRowDto> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op.action, Action::Conflict | Action::Note))
        .map(|(index, _)| SelectedRowDto {
            index,
            flipped: false,
        })
        .collect();
    if selected.is_empty() {
        return Err(
            "AutoScan found no executable operations; unattended Apply will not run a no-op plan"
                .into(),
        );
    }
    Ok(selected)
}

fn prepare_autoscan_apply(
    results: &CompareResultRepository,
    ticket: &AutoApplyTicket,
) -> Result<PreparedApply, String> {
    let retained_plan = load_retained_apply(results, ticket.compare_identity())?;
    if retained_plan.owner.identity != *ticket.compare_identity() {
        return Err("The completed AutoScan ticket no longer owns this Compare result".into());
    }
    let selected = server_owned_selection(&retained_plan.plan.ops)?;
    prepare_retained_apply(retained_plan, selected)
}

fn apply_facts(prepared: &PreparedApply) -> Result<ApplyFacts, String> {
    let requirements = run::apply_requirements(
        &prepared.target.target_job,
        &prepared.plan,
        &prepared.selected_operations,
        false,
    )
    .map_err(format_run_io_error)?;
    let acknowledged = if requirements.verdict.ok() {
        requirements.verdict.clone()
    } else {
        run::preflight(
            &prepared.target.target_job,
            &prepared.plan,
            &prepared.selected_operations,
            true,
        )
        .map_err(format_run_io_error)?
    };
    Ok(ApplyFacts {
        unacknowledged: requirements.verdict,
        acknowledged,
        capabilities: requirements.capabilities,
    })
}

fn build_apply_review(prepared: &PreparedApply, facts: &ApplyFacts) -> Result<ApplyReview, String> {
    ApplyReview::new(
        prepared.owner.identity.clone(),
        prepared.plan_digest.clone(),
        prepared.selected_rows.clone(),
        health_review_digest(&facts.unacknowledged, &facts.acknowledged),
        facts
            .capabilities
            .consent_digest(CapabilityScope::ApplyWrite),
    )
}

fn apply_review_messages(facts: &ApplyFacts) -> (Vec<String>, Vec<String>, bool) {
    let requires_health_ack = !facts.unacknowledged.ok() && facts.acknowledged.ok();
    let mut blockers = facts.acknowledged.blockers.clone();
    blockers.extend(capability_blockers(&facts.capabilities));
    let mut warnings = facts.unacknowledged.warnings.clone();
    if requires_health_ack {
        warnings.extend(facts.unacknowledged.blockers.clone());
    } else {
        warnings.extend(facts.acknowledged.warnings.clone());
    }
    warnings.sort();
    warnings.dedup();
    (blockers, warnings, requires_health_ack)
}

fn autoscan_health_refusals(facts: &ApplyFacts) -> Vec<String> {
    let mut messages = facts.unacknowledged.blockers.clone();
    messages.extend(facts.unacknowledged.warnings.clone());
    messages.sort();
    messages.dedup();
    messages
}

#[tauri::command]
pub async fn review_apply(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    compare_identity: CompareIdentity,
    selected: Vec<SelectedRowDto>,
) -> Result<OperationReviewDto, String> {
    require_main_window(&window)?;
    let _command = lifecycle.inner().command_lease()?;
    let results = results.inner().clone();
    let authorizations = authorizations.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let prepared = prepare_apply(&results, &compare_identity, selected)?;
        let facts = match apply_facts(&prepared) {
            Ok(facts) => facts,
            Err(error) => {
                return Ok(blocked_review(
                    vec![error],
                    Vec::new(),
                    &CapReport::default(),
                ))
            }
        };
        let (blockers, warnings, requires_health_ack) = apply_review_messages(&facts);
        if !blockers.is_empty() {
            return Ok(blocked_review(blockers, warnings, &facts.capabilities));
        }
        let review = build_apply_review(&prepared, &facts)?;
        let requires_capability_ack = !facts.capabilities.needs_ack().is_empty()
            && !authorizations.has_interactive_apply_capability_grant(&review);
        let compare_identity = review.compare_identity().clone();
        let challenge = results.with_fresh_execution_eligibility(&compare_identity, || {
            authorizations.create_review_challenge(ReviewChallenge::InteractiveApply {
                review,
                requires_health_ack,
                requires_capability_ack,
            })
        })?;
        Ok(OperationReviewDto::InteractiveApplyConfirmationRequired {
            challenge_id: challenge.challenge_id,
            expires_at_ms: challenge.expires_at_ms,
            warnings,
            capabilities: capability_dtos(&facts.capabilities),
            requires_health_ack,
            requires_capability_ack,
            can_remember_for_session: true,
            can_allow_unattended: true,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn authorize_autoscan_apply(
    window: tauri::WebviewWindow,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    generation: u64,
    ticket_id: u64,
) -> Result<AuthorizationDto, String> {
    require_main_window(&window)?;
    let _command = lifecycle.inner().command_lease()?;
    let results = results.inner().clone();
    let authorizations = authorizations.inner().clone();
    let autoscan = autoscan.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let ticket = autoscan.claim_completed_auto_apply(generation, ticket_id)?;
        let prepared = prepare_autoscan_apply(&results, &ticket)?;
        let facts = apply_facts(&prepared)?;
        let health_refusals = autoscan_health_refusals(&facts);
        if !health_refusals.is_empty() {
            return Err(format!(
                "AutoScan Apply requires a completely clean health review:\n{}",
                health_refusals.join("\n")
            ));
        }
        let blockers = capability_blockers(&facts.capabilities);
        if !blockers.is_empty() {
            return Err(blockers.join("\n"));
        }
        reload_prepared_target(&prepared.target)?;
        let review = build_apply_review(&prepared, &facts)?;
        let compare_identity = review.compare_identity().clone();
        let issued = autoscan.authorize_claim(&ticket, || {
            results.with_fresh_execution_eligibility(&compare_identity, || {
                authorizations.issue_auto_apply_authorization(review, ticket.clone())
            })
        })?;
        Ok(authorization_dto(issued))
    })
    .await
    .map_err(|error| error.to_string())?
}

fn revalidate_retained_before_apply(
    results: &CompareResultRepository,
    prepared: &PreparedApply,
    command: &RunCommandLease,
    launch: ApplyLaunch,
) -> Result<ActiveRunLease, String> {
    // Root/capability probes above can be slow. Re-read the registry after them, without holding
    // any result/auth/run lock, so an external TOML edit cannot ride an old in-memory Job into the
    // reservation. The core reopens roots once more after reservation and enforces exact consent.
    let _current = reload_prepared_target(&prepared.target)?;
    let retained = results
        .get_exact(&prepared.owner.identity)
        .map_err(|error| error.to_string())?;
    validate_retained_compare(
        retained.as_ref(),
        &prepared.owner,
        &prepared.target.registered_job.job_id,
        &prepared.target.job_name,
        prepared.target.target_index,
        &prepared.target.config_revision,
        Some(&prepared.plan_digest),
    )?;
    results.with_fresh_execution_eligibility(&prepared.owner.identity, || match launch {
        ApplyLaunch::Interactive { progress_launch_id } => {
            command.start_apply_from_progress_launch(progress_launch_id)
        }
        ApplyLaunch::AutoScan => command.start_run(RunPurpose::Apply),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyLaunch {
    Interactive { progress_launch_id: u64 },
    AutoScan,
}

impl ApplyLaunch {
    fn bind(
        authorization_kind: ApplyAuthorizationKind,
        progress_launch_id: Option<u64>,
    ) -> Result<Self, String> {
        match (authorization_kind, progress_launch_id) {
            (ApplyAuthorizationKind::Interactive, Some(progress_launch_id)) => {
                Ok(Self::Interactive { progress_launch_id })
            }
            (ApplyAuthorizationKind::AutoScan, None) => Ok(Self::AutoScan),
            (ApplyAuthorizationKind::Interactive, None) => Err(
                "Interactive Apply requires its reserved progress window — review Apply again"
                    .into(),
            ),
            (ApplyAuthorizationKind::AutoScan, Some(_)) => {
                Err("AutoScan Apply cannot use an interactive progress-window launch".into())
            }
        }
    }
}

#[allow(clippy::too_many_arguments)] // Tauri injects state and exposes the rest as flat IPC fields.
#[tauri::command]
pub async fn apply_job(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    lifecycle: tauri::State<'_, Arc<RunLifecycle>>,
    results: tauri::State<'_, Arc<CompareResultRepository>>,
    events: tauri::State<'_, Arc<RunEventRepository>>,
    authorizations: tauri::State<'_, Arc<OperationAuthorizationStore>>,
    autoscan: tauri::State<'_, Arc<AutoScanController>>,
    authorization_token: String,
    launch_id: Option<u64>,
) -> Result<ApplyDto, String> {
    require_main_window(&window)?;
    let lifecycle = lifecycle.inner().clone();
    let command = lifecycle.command_lease()?;
    let results = results.inner().clone();
    let events = events.inner().clone();
    let authorizations = authorizations.inner().clone();
    let autoscan = autoscan.inner().clone();
    let rejection_lifecycle = lifecycle.clone();
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
                reviewed.selected_rows().to_vec(),
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
                results.clone(),
                &prepared.target.registered_job.job_id,
                &prepared.target.config_revision,
            );
            let ctx = make_ctx(&run_app, events, run_id, ctl, "apply");
            let t0 = std::time::Instant::now();
            let recorder = syncdash::obs::runlog::Recorder::start(
                &prepared.target.job_name,
                &run::run_kind(&prepared.target.target_job, "apply"),
                &ctx,
                &prepared.selected_operations,
            );
            let execution = run::apply_with_capability_consent_classified(
                &prepared.target.job_name,
                &prepared.target.target_job,
                &prepared.plan,
                &prepared.selected_operations,
                None,
                false,
                health_warning_acknowledged,
                &consent,
                &recorder.ctx,
            );
            if !execution.writes_started() {
                applied_result.retain_for_safe_rejection();
            }
            let outcome = match execution.into_result() {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Err((format_run_io_error(error), true));
                }
            };
            recorder.finish(&outcome, t0.elapsed().as_millis() as u64);
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
                let _ = app.emit(
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
                        let _ = app.emit(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn op(action: Action, path: &str) -> Op {
        Op {
            side: syncdash::model::plan::Side::Target,
            action,
            path: path.into(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "test".into(),
        }
    }

    fn resolved_target(name: &str, revision: &str, target_index: usize) -> ResolvedJobTarget {
        let registered_job = syncdash::job::Job {
            job_id: "job-a".into(),
            ..Default::default()
        };
        ResolvedJobTarget {
            job_name: name.into(),
            target_job: registered_job.clone(),
            registered_job,
            target_index,
            config_revision: revision.into(),
        }
    }

    #[test]
    fn registry_identity_is_rechecked_after_slow_probes_before_reservation() {
        let reviewed = resolved_target("photos", "revision-a", 1);
        assert!(validate_resolved_target_unchanged(&reviewed, &reviewed).is_ok());

        let renamed = resolved_target("archive", "revision-a", 1);
        assert!(validate_resolved_target_unchanged(&reviewed, &renamed).is_ok());
        let revised = resolved_target("photos", "revision-b", 1);
        assert!(validate_resolved_target_unchanged(&reviewed, &revised).is_err());
        let retargeted = resolved_target("photos", "revision-a", 0);
        assert!(validate_resolved_target_unchanged(&reviewed, &retargeted).is_err());
        let mut replaced = resolved_target("photos", "revision-a", 1);
        replaced.registered_job.job_id = "job-b".into();
        assert!(validate_resolved_target_unchanged(&reviewed, &replaced).is_err());
    }

    #[test]
    fn autoscan_selection_is_server_owned_complete_ordered_and_never_flipped() {
        let ops = vec![
            op(Action::Copy, "copy"),
            op(Action::Conflict, "conflict"),
            op(Action::Note, "note"),
            op(Action::Delete, "delete"),
        ];
        let selected = server_owned_selection(&ops).unwrap();
        assert_eq!(
            selected,
            vec![
                SelectedRowDto {
                    index: 0,
                    flipped: false,
                },
                SelectedRowDto {
                    index: 3,
                    flipped: false,
                },
            ]
        );
        assert!(server_owned_selection(&ops[1..3]).is_err());
    }

    #[test]
    fn autoscan_health_refuses_warning_only_verdicts_deterministically() {
        let facts = ApplyFacts {
            unacknowledged: Verdict {
                blockers: Vec::new(),
                warnings: vec!["z warning".into(), "a warning".into(), "a warning".into()],
            },
            acknowledged: Verdict {
                blockers: Vec::new(),
                warnings: Vec::new(),
            },
            capabilities: CapReport::default(),
        };
        assert_eq!(
            autoscan_health_refusals(&facts),
            vec!["a warning".to_string(), "z warning".to_string()]
        );
    }

    #[test]
    fn apply_authorization_kind_requires_the_matching_launch_channel() {
        assert_eq!(
            ApplyLaunch::bind(ApplyAuthorizationKind::Interactive, Some(17)).unwrap(),
            ApplyLaunch::Interactive {
                progress_launch_id: 17
            }
        );
        assert_eq!(
            ApplyLaunch::bind(ApplyAuthorizationKind::AutoScan, None).unwrap(),
            ApplyLaunch::AutoScan
        );
        assert!(ApplyLaunch::bind(ApplyAuthorizationKind::Interactive, None).is_err());
        assert!(ApplyLaunch::bind(ApplyAuthorizationKind::AutoScan, Some(17)).is_err());
    }
}
