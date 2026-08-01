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

use crate::auth::{
    health_digest, AuthorizationPurpose, AuthorizationRecord, AuthorizationStore, ChallengeSpec,
    OperationBinding,
};
use crate::bridge::{make_ctx, RunEvent, RunEventRepository};
use crate::dto::{
    ApplyDto, AuthorizationDto, CapabilityIssueDto, CompareOwner, OperationReviewDto, PlanDto,
    ReviewStatus, SelectedRowDto,
};
use crate::state::{
    begin_run, begin_run_command, begin_run_for_launch, end_run, finish_run_command,
    release_progress_launch, request_cancel, resolve_selected_ops, resolve_target, set_paused,
    user_err, validate_cached_compare, CachedCompare, CompareProvenance, ResultKey,
    ResultRepository, RunState,
};

use super::require_main_window;

#[derive(Clone, serde::Serialize)]
struct RunRejected {
    launch_id: u64,
    message: String,
}

struct ActiveRunGuard {
    state: Arc<RunState>,
    run_id: u64,
}

struct RunCommandGuard(Arc<RunState>);

struct AppliedResultGuard {
    results: Arc<ResultRepository>,
    job_id: String,
    config_revision: String,
    invalidate_on_drop: bool,
}

impl AppliedResultGuard {
    fn new(results: Arc<ResultRepository>, job_id: &str, config_revision: &str) -> Self {
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
                .0
                .lock()
                .unwrap()
                .invalidate_revision(&self.job_id, &self.config_revision);
        }
    }
}

impl RunCommandGuard {
    fn begin(state: Arc<RunState>) -> Self {
        begin_run_command(&state);
        Self(state)
    }
}

impl Drop for RunCommandGuard {
    fn drop(&mut self) {
        finish_run_command(&self.0);
    }
}

impl Drop for ActiveRunGuard {
    fn drop(&mut self) {
        end_run(&self.state, self.run_id);
    }
}

/// Request cooperative cancellation of the active run. Returns whether an active run existed.
#[tauri::command]
pub fn cancel_run(state: tauri::State<'_, Arc<RunState>>, run_id: u64) -> Result<bool, String> {
    request_cancel(state.inner(), run_id)
}

/// Pause/resume the active run (elapsed stops growing while paused, the RootLock heartbeat keeps beating)
#[tauri::command]
pub fn pause_run(
    state: tauri::State<'_, Arc<RunState>>,
    run_id: u64,
    paused: bool,
) -> Result<bool, String> {
    set_paused(state.inner(), run_id, paused)
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

struct LoadedTarget {
    job_name: String,
    full_job: syncdash::job::Job,
    target_index: usize,
    job: syncdash::job::Job,
    config_revision: String,
}

struct PreparedApply {
    loaded: LoadedTarget,
    owner: CompareOwner,
    plan: Plan,
    plan_digest: String,
    selected: Vec<SelectedRowDto>,
    ops: Vec<Op>,
}

struct ApplyFacts {
    unacknowledged: Verdict,
    acknowledged: Verdict,
    capabilities: CapReport,
}

fn load_review_target(
    name: &str,
    expected_job_id: &str,
    target_index: Option<usize>,
) -> Result<LoadedTarget, String> {
    let (job_name, full_job) = job::load_named(name).map_err(|error| error.to_string())?;
    if full_job.job_id != expected_job_id {
        return Err(format!(
            "Job '{job_name}' was replaced — refresh it and review Compare again"
        ));
    }
    load_target(job_name, full_job, target_index)
}

fn load_bound_target(binding: &OperationBinding) -> Result<LoadedTarget, String> {
    let (job_name, full_job) = job::load_by_id(&binding.job_id).map_err(|error| {
        format!("The authorized job was deleted or replaced — review the operation again: {error}")
    })?;
    let loaded = load_target(job_name, full_job, Some(binding.target_index))?;
    if loaded.config_revision != binding.config_revision {
        return Err(format!(
            "Job '{}' changed after authorization — review the operation again",
            loaded.job_name
        ));
    }
    Ok(loaded)
}

fn load_target(
    job_name: String,
    full_job: syncdash::job::Job,
    target_index: Option<usize>,
) -> Result<LoadedTarget, String> {
    let config_revision =
        job::config_revision(&full_job).map_err(|error| format!("Job '{job_name}': {error}"))?;
    let (target_index, job) = resolve_target(&full_job, target_index)?;
    Ok(LoadedTarget {
        job_name,
        full_job,
        target_index,
        job,
        config_revision,
    })
}

fn validate_loaded_target_unchanged(
    reviewed: &LoadedTarget,
    current: &LoadedTarget,
) -> Result<(), String> {
    if reviewed.full_job.job_id != current.full_job.job_id {
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

fn reload_prepared_target(reviewed: &LoadedTarget) -> Result<LoadedTarget, String> {
    let (job_name, full_job) = job::load_by_id(&reviewed.full_job.job_id).map_err(|error| {
        format!("The reviewed job was deleted or replaced — review again: {error}")
    })?;
    let current = load_target(job_name, full_job, Some(reviewed.target_index))?;
    validate_loaded_target_unchanged(reviewed, &current)?;
    Ok(current)
}

fn empty_verdict() -> Verdict {
    Verdict {
        blockers: Vec::new(),
        warnings: Vec::new(),
    }
}

fn compare_binding(loaded: &LoadedTarget, capabilities: &CapReport) -> OperationBinding {
    let empty = empty_verdict();
    OperationBinding {
        scope: CapabilityScope::CompareRead,
        purpose: AuthorizationPurpose::CompareInteractive,
        job_id: loaded.full_job.job_id.clone(),
        job_name: loaded.job_name.clone(),
        config_revision: loaded.config_revision.clone(),
        target_index: loaded.target_index,
        owner: None,
        plan_digest: None,
        decision_digest: None,
        health_digest: health_digest(&empty, &empty),
        capability_digest: capabilities.consent_digest(CapabilityScope::CompareRead),
    }
}

fn authorization_dto(record: AuthorizationRecord, expires_at_ms: u64) -> AuthorizationDto {
    AuthorizationDto {
        authorization_token: record.token,
        expires_at_ms,
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
    OperationReviewDto {
        status: ReviewStatus::Blocked,
        authorization: None,
        challenge_id: None,
        expires_at_ms: None,
        blockers,
        warnings,
        capabilities: capability_dtos(capabilities),
        requires_health_ack: false,
        requires_capability_ack: false,
        can_remember_for_session: false,
        can_allow_unattended: false,
    }
}

fn validate_exact_binding(
    authorized: &OperationBinding,
    current: &OperationBinding,
) -> Result<(), String> {
    if authorized.scope != current.scope || authorized.purpose != current.purpose {
        return Err("The authorization belongs to a different operation".into());
    }
    if authorized.job_id != current.job_id {
        return Err("The authorized job identity changed — review again".into());
    }
    if authorized.config_revision != current.config_revision {
        return Err("The authorized job configuration changed — review again".into());
    }
    if authorized.target_index != current.target_index {
        return Err("The authorized target changed — review again".into());
    }
    if !same_compare_identity(authorized.owner.as_ref(), current.owner.as_ref()) {
        return Err("The authorized Compare result changed — review Apply again".into());
    }
    if authorized.plan_digest != current.plan_digest {
        return Err("The authorized plan changed — review Apply again".into());
    }
    if authorized.decision_digest != current.decision_digest {
        return Err("The authorized selected operation set changed — review Apply again".into());
    }
    if authorized.health_digest != current.health_digest {
        return Err("The plan health report changed — review the operation again".into());
    }
    if authorized.capability_digest != current.capability_digest {
        return Err("The backend capability report changed — review the operation again".into());
    }
    Ok(())
}

fn same_compare_identity(left: Option<&CompareOwner>, right: Option<&CompareOwner>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.compare_id == right.compare_id
                && left.job_id == right.job_id
                && left.target_index == right.target_index
                && left.config_revision == right.config_revision
        }
        _ => false,
    }
}

#[tauri::command]
pub async fn review_compare(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Arc<RunState>>,
    authorizations: tauri::State<'_, Arc<AuthorizationStore>>,
    name: String,
    expected_job_id: String,
    target_index: Option<usize>,
) -> Result<OperationReviewDto, String> {
    require_main_window(&window)?;
    let st = state.inner().clone();
    let _command = RunCommandGuard::begin(st);
    let authorizations = authorizations.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let loaded = load_review_target(&name, &expected_job_id, target_index)?;
        let capabilities = match run::compare_capabilities(&loaded.job) {
            Ok(capabilities) => capabilities,
            Err(error) => {
                return Ok(blocked_review(
                    vec![user_err(error)],
                    Vec::new(),
                    &CapReport::default(),
                ))
            }
        };
        let blockers = capability_blockers(&capabilities);
        if !blockers.is_empty() {
            return Ok(blocked_review(blockers, Vec::new(), &capabilities));
        }
        let binding = compare_binding(&loaded, &capabilities);
        let requires_capability_ack = !capabilities.needs_ack().is_empty();
        if !requires_capability_ack || authorizations.grant_allows(&binding, false) {
            let (authorization, expires_at_ms) =
                authorizations.authorize_direct(binding, Vec::new(), false)?;
            return Ok(OperationReviewDto {
                status: ReviewStatus::DirectAuthorized,
                authorization: Some(authorization_dto(authorization, expires_at_ms)),
                challenge_id: None,
                expires_at_ms: None,
                blockers: Vec::new(),
                warnings: Vec::new(),
                capabilities: capability_dtos(&capabilities),
                requires_health_ack: false,
                requires_capability_ack: false,
                can_remember_for_session: false,
                can_allow_unattended: false,
            });
        }
        let (challenge_id, expires_at_ms) = authorizations.challenge(ChallengeSpec {
            binding,
            selected: Vec::new(),
            requires_health_ack: false,
            requires_capability_ack: true,
        })?;
        Ok(OperationReviewDto {
            status: ReviewStatus::ConfirmationRequired,
            authorization: None,
            challenge_id: Some(challenge_id),
            expires_at_ms: Some(expires_at_ms),
            blockers: Vec::new(),
            warnings: Vec::new(),
            capabilities: capability_dtos(&capabilities),
            requires_health_ack: false,
            requires_capability_ack: true,
            can_remember_for_session: true,
            can_allow_unattended: false,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub fn approve_operation(
    window: tauri::WebviewWindow,
    authorizations: tauri::State<'_, Arc<AuthorizationStore>>,
    challenge_id: String,
    acknowledge_health: bool,
    accept_capabilities: bool,
    remember_for_session: bool,
    allow_unattended: bool,
) -> Result<AuthorizationDto, String> {
    require_main_window(&window)?;
    let (authorization, expires_at_ms) = authorizations.approve(
        &challenge_id,
        acknowledge_health,
        accept_capabilities,
        remember_for_session,
        allow_unattended,
    )?;
    Ok(authorization_dto(authorization, expires_at_ms))
}

#[tauri::command]
pub async fn compare_job(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<RunState>>,
    results: tauri::State<'_, Arc<ResultRepository>>,
    events: tauri::State<'_, Arc<RunEventRepository>>,
    authorizations: tauri::State<'_, Arc<AuthorizationStore>>,
    authorization_token: String,
) -> Result<PlanDto, String> {
    require_main_window(&window)?;
    let st = state.inner().clone();
    let _command = RunCommandGuard::begin(st.clone());
    let results = results.inner().clone();
    let events = events.inner().clone();
    let authorizations = authorizations.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let authorization = authorizations.consume(
            &authorization_token,
            AuthorizationPurpose::CompareInteractive,
        )?;
        let loaded = load_bound_target(&authorization.binding)?;
        let capabilities = run::compare_capabilities(&loaded.job).map_err(user_err)?;
        let blockers = capability_blockers(&capabilities);
        if !blockers.is_empty() {
            return Err(blockers.join("\n"));
        }
        let loaded = reload_prepared_target(&loaded)?;
        let current_binding = compare_binding(&loaded, &capabilities);
        validate_exact_binding(&authorization.binding, &current_binding)?;
        let consent = CapabilityConsent::ExactDigest(current_binding.capability_digest.clone());

        let (run_id, ctl) = begin_run(&st)?;
        let _active_run = ActiveRunGuard {
            state: st.clone(),
            run_id,
        };
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
        let result =
            run::compare_with_capability_consent(&loaded.job_name, &loaded.job, &ctx, &consent);
        syncdash::obs::runlog::compare_summary(
            &loaded.job_name,
            &run::run_kind(&loaded.job, "compare"),
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
        let outcome = result.map_err(user_err)?;
        let plan_digest = outcome.plan.digest();
        let mut owner = CompareOwner {
            compare_id: run_id,
            job_id: loaded.full_job.job_id.clone(),
            job_name: loaded.job_name.clone(),
            target_index: loaded.target_index,
            config_revision: loaded.config_revision.clone(),
        };
        let evidence = compare::evidence::evidence(
            &outcome.source,
            &outcome.target,
            &outcome.plan,
            &loaded.job.compare_opts(),
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
            equal_count: evidence.equal_count,
            equal_bytes: evidence.equal_bytes,
        };

        let mut repository = results.0.lock().unwrap();
        let (current_name, current_job) = job::load_by_id(&owner.job_id).map_err(|error| {
            format!(
                "Job '{}' was deleted or replaced while Compare was running: {error}",
                owner.job_name
            )
        })?;
        let current_revision = job::config_revision(&current_job)
            .map_err(|error| format!("Job '{}': {error}", owner.job_name))?;
        if current_revision != owner.config_revision {
            return Err(format!(
                "Job '{}' changed while Compare was running — run Compare again",
                owner.job_name
            ));
        }
        owner.job_name = current_name;
        dto.owner = owner.clone();
        repository.insert(CachedCompare {
            provenance: CompareProvenance { owner, plan_digest },
            plan: dto.clone(),
            source: outcome.source,
            target: outcome.target,
        });
        Ok(dto)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn prepare_apply(
    results: &ResultRepository,
    requested_owner: &CompareOwner,
    selected: Vec<SelectedRowDto>,
) -> Result<PreparedApply, String> {
    let (job_name, full_job) = job::load_by_id(&requested_owner.job_id).map_err(|error| {
        format!("The Compare result's job was deleted or replaced — run Compare again: {error}")
    })?;
    let loaded = load_target(job_name, full_job, Some(requested_owner.target_index))?;
    if loaded.config_revision != requested_owner.config_revision {
        return Err(format!(
            "Job '{}' changed since this Compare — run Compare again",
            loaded.job_name
        ));
    }
    let (owner, header, plan_ops, plan_digest) = {
        let mut repository = results.0.lock().unwrap();
        let key = ResultKey::new(
            &loaded.full_job.job_id,
            loaded.target_index,
            &loaded.config_revision,
        );
        let cached = repository.get(&key);
        validate_cached_compare(
            cached.map(|result| &result.provenance),
            requested_owner,
            &loaded.full_job.job_id,
            &loaded.job_name,
            loaded.target_index,
            &loaded.config_revision,
            None,
        )?;
        let cached = cached.expect("validated cached compare must exist");
        (
            cached.provenance.owner.clone(),
            cached.plan.header.clone(),
            cached.plan.ops.clone(),
            cached.provenance.plan_digest.clone(),
        )
    };
    let plan = Plan {
        header,
        ops: plan_ops,
    };
    if plan.digest() != plan_digest {
        return Err("The cached Compare plan changed — run Compare again".into());
    }
    let ops = resolve_selected_ops(&plan.ops, &selected)?;
    Ok(PreparedApply {
        loaded,
        owner,
        plan,
        plan_digest,
        selected,
        ops,
    })
}

fn apply_facts(prepared: &PreparedApply) -> Result<ApplyFacts, String> {
    let requirements =
        run::apply_requirements(&prepared.loaded.job, &prepared.plan, &prepared.ops, false)
            .map_err(user_err)?;
    let acknowledged = if requirements.verdict.ok() {
        requirements.verdict.clone()
    } else {
        run::preflight(&prepared.loaded.job, &prepared.plan, &prepared.ops, true)
            .map_err(user_err)?
    };
    Ok(ApplyFacts {
        unacknowledged: requirements.verdict,
        acknowledged,
        capabilities: requirements.capabilities,
    })
}

fn apply_binding(
    prepared: &PreparedApply,
    facts: &ApplyFacts,
    purpose: AuthorizationPurpose,
) -> Result<OperationBinding, String> {
    Ok(OperationBinding {
        scope: CapabilityScope::ApplyWrite,
        purpose,
        job_id: prepared.loaded.full_job.job_id.clone(),
        job_name: prepared.loaded.job_name.clone(),
        config_revision: prepared.loaded.config_revision.clone(),
        target_index: prepared.loaded.target_index,
        owner: Some(prepared.owner.clone()),
        plan_digest: Some(prepared.plan_digest.clone()),
        decision_digest: Some(crate::auth::decision_digest(&prepared.selected)?),
        health_digest: health_digest(&facts.unacknowledged, &facts.acknowledged),
        capability_digest: facts
            .capabilities
            .consent_digest(CapabilityScope::ApplyWrite),
    })
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

#[tauri::command]
pub async fn review_apply(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Arc<RunState>>,
    results: tauri::State<'_, Arc<ResultRepository>>,
    authorizations: tauri::State<'_, Arc<AuthorizationStore>>,
    owner: CompareOwner,
    selected: Vec<SelectedRowDto>,
) -> Result<OperationReviewDto, String> {
    require_main_window(&window)?;
    let st = state.inner().clone();
    let _command = RunCommandGuard::begin(st);
    let results = results.inner().clone();
    let authorizations = authorizations.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let prepared = prepare_apply(&results, &owner, selected)?;
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
        let requires_capability_ack = !facts.capabilities.needs_ack().is_empty();
        let binding = apply_binding(&prepared, &facts, AuthorizationPurpose::ApplyInteractive)?;
        let (challenge_id, expires_at_ms) = authorizations.challenge(ChallengeSpec {
            binding,
            selected: prepared.selected,
            requires_health_ack,
            requires_capability_ack,
        })?;
        Ok(OperationReviewDto {
            status: ReviewStatus::ConfirmationRequired,
            authorization: None,
            challenge_id: Some(challenge_id),
            expires_at_ms: Some(expires_at_ms),
            blockers: Vec::new(),
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
pub async fn authorize_unattended_apply(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Arc<RunState>>,
    results: tauri::State<'_, Arc<ResultRepository>>,
    authorizations: tauri::State<'_, Arc<AuthorizationStore>>,
    owner: CompareOwner,
    selected: Vec<SelectedRowDto>,
) -> Result<AuthorizationDto, String> {
    require_main_window(&window)?;
    let st = state.inner().clone();
    let _command = RunCommandGuard::begin(st);
    let results = results.inner().clone();
    let authorizations = authorizations.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let prepared = prepare_apply(&results, &owner, selected)?;
        let facts = apply_facts(&prepared)?;
        if !facts.unacknowledged.ok() {
            return Err(format!(
                "Unattended Apply cannot acknowledge plan-health warnings:\n{}",
                facts.unacknowledged.blockers.join("\n")
            ));
        }
        let blockers = capability_blockers(&facts.capabilities);
        if !blockers.is_empty() {
            return Err(blockers.join("\n"));
        }
        reload_prepared_target(&prepared.loaded)?;
        let binding = apply_binding(&prepared, &facts, AuthorizationPurpose::ApplyUnattended)?;
        let (authorization, expires_at_ms) =
            authorizations.authorize_unattended(binding, prepared.selected)?;
        Ok(authorization_dto(authorization, expires_at_ms))
    })
    .await
    .map_err(|error| error.to_string())?
}

fn revalidate_cached_before_apply(
    results: &ResultRepository,
    prepared: &PreparedApply,
    state: &RunState,
    launch_id: Option<u64>,
) -> Result<(u64, Arc<syncdash::obs::progress::RunCtl>), String> {
    // Root/capability probes above can be slow. Re-read the registry after them, without holding
    // any result/auth/run lock, so an external TOML edit cannot ride an old in-memory Job into the
    // reservation. The core reopens roots once more after reservation and enforces exact consent.
    let _current = reload_prepared_target(&prepared.loaded)?;
    let mut repository = results.0.lock().unwrap();
    let key = ResultKey::new(
        &prepared.loaded.full_job.job_id,
        prepared.loaded.target_index,
        &prepared.loaded.config_revision,
    );
    let cached = repository.get(&key);
    validate_cached_compare(
        cached.map(|result| &result.provenance),
        &prepared.owner,
        &prepared.loaded.full_job.job_id,
        &prepared.loaded.job_name,
        prepared.loaded.target_index,
        &prepared.loaded.config_revision,
        Some(&prepared.plan_digest),
    )?;
    begin_run_for_launch(state, launch_id)
}

#[tauri::command]
pub async fn apply_job(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<RunState>>,
    results: tauri::State<'_, Arc<ResultRepository>>,
    events: tauri::State<'_, Arc<RunEventRepository>>,
    authorizations: tauri::State<'_, Arc<AuthorizationStore>>,
    authorization_token: String,
    launch_id: Option<u64>,
) -> Result<ApplyDto, String> {
    require_main_window(&window)?;
    let st = state.inner().clone();
    let _command = RunCommandGuard::begin(st.clone());
    let results = results.inner().clone();
    let events = events.inner().clone();
    let authorizations = authorizations.inner().clone();
    let reject_state = state.inner().clone();
    let requested_launch = launch_id;
    let run_app = app.clone();
    let joined =
        tauri::async_runtime::spawn_blocking(move || -> Result<ApplyDto, (String, bool)> {
            let authorization = authorizations
                .consume_apply(&authorization_token)
                .map_err(|error| (error, false))?;
            let purpose = authorization.binding.purpose;
            let owner =
                authorization.binding.owner.clone().ok_or_else(|| {
                    ("The Apply authorization has no Compare owner".into(), false)
                })?;
            let prepared = prepare_apply(&results, &owner, authorization.selected)
                .map_err(|error| (error, false))?;
            let facts = apply_facts(&prepared).map_err(|error| (error, false))?;
            let current_binding =
                apply_binding(&prepared, &facts, purpose).map_err(|error| (error, false))?;
            validate_exact_binding(&authorization.binding, &current_binding)
                .map_err(|error| (error, false))?;
            let blockers = capability_blockers(&facts.capabilities);
            if !blockers.is_empty() {
                return Err((blockers.join("\n"), false));
            }
            if purpose == AuthorizationPurpose::ApplyUnattended && authorization.acknowledged_health
            {
                return Err((
                    "An unattended Apply cannot acknowledge plan-health warnings".into(),
                    false,
                ));
            }
            let verdict = if authorization.acknowledged_health {
                &facts.acknowledged
            } else {
                &facts.unacknowledged
            };
            if !verdict.ok() {
                return Err((verdict.blockers.join("\n"), false));
            }
            let consent = CapabilityConsent::ExactDigest(current_binding.capability_digest.clone());
            let (run_id, ctl) = revalidate_cached_before_apply(&results, &prepared, &st, launch_id)
                .map_err(|error| (error, false))?;
            let mut applied_result = AppliedResultGuard::new(
                results.clone(),
                &prepared.loaded.full_job.job_id,
                &prepared.loaded.config_revision,
            );
            let _active_run = ActiveRunGuard {
                state: st.clone(),
                run_id,
            };
            let ctx = make_ctx(&run_app, events, run_id, ctl, "apply");
            let t0 = std::time::Instant::now();
            let recorder = syncdash::obs::runlog::Recorder::start(
                &prepared.loaded.job_name,
                &run::run_kind(&prepared.loaded.job, "apply"),
                &ctx,
                &prepared.ops,
            );
            let execution = run::apply_with_capability_consent_classified(
                &prepared.loaded.job_name,
                &prepared.loaded.job,
                &prepared.plan,
                &prepared.ops,
                None,
                false,
                authorization.acknowledged_health,
                &consent,
                &recorder.ctx,
            );
            if !execution.writes_started() {
                applied_result.retain_for_safe_rejection();
            }
            let outcome = match execution.into_result() {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Err((user_err(error), true));
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
                release_progress_launch(&reject_state, launch_id);
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
                    if release_progress_launch(&reject_state, launch_id) {
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

    fn owner() -> CompareOwner {
        CompareOwner {
            compare_id: 7,
            job_id: "job-a".into(),
            job_name: "photos".into(),
            target_index: 1,
            config_revision: "revision-a".into(),
        }
    }

    fn binding() -> OperationBinding {
        OperationBinding {
            scope: CapabilityScope::ApplyWrite,
            purpose: AuthorizationPurpose::ApplyInteractive,
            job_id: "job-a".into(),
            job_name: "photos".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
            owner: Some(owner()),
            plan_digest: Some("plan-a".into()),
            decision_digest: Some("selection-a".into()),
            health_digest: "health-a".into(),
            capability_digest: "caps-a".into(),
        }
    }

    fn loaded(name: &str, revision: &str, target_index: usize) -> LoadedTarget {
        let mut full_job = syncdash::job::Job::default();
        full_job.job_id = "job-a".into();
        LoadedTarget {
            job_name: name.into(),
            job: full_job.clone(),
            full_job,
            target_index,
            config_revision: revision.into(),
        }
    }

    #[test]
    fn active_run_guard_releases_state_during_unwind() {
        let state = Arc::new(RunState::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let state = state.clone();
            move || {
                let (run_id, _) = begin_run(&state).unwrap();
                let _active_run = ActiveRunGuard {
                    state: state.clone(),
                    run_id,
                };
                panic!("worker panic");
            }
        }));

        assert!(result.is_err());
        assert!(begin_run(&state).is_ok());
    }

    #[test]
    fn every_execution_fingerprint_is_exact() {
        let expected = binding();
        assert!(validate_exact_binding(&expected, &expected).is_ok());

        let mut renamed = expected.clone();
        renamed.job_name = "archive".into();
        renamed.owner.as_mut().unwrap().job_name = "archive".into();
        assert!(validate_exact_binding(&expected, &renamed).is_ok());

        let mut changed = expected.clone();
        changed.job_id = "job-b".into();
        assert!(validate_exact_binding(&expected, &changed).is_err());
        changed = expected.clone();
        changed.config_revision = "revision-b".into();
        assert!(validate_exact_binding(&expected, &changed).is_err());
        changed = expected.clone();
        changed.target_index = 0;
        assert!(validate_exact_binding(&expected, &changed).is_err());
        changed = expected.clone();
        changed.capability_digest = "caps-b".into();
        assert!(validate_exact_binding(&expected, &changed).is_err());
        changed = expected.clone();
        changed.plan_digest = Some("plan-b".into());
        assert!(validate_exact_binding(&expected, &changed).is_err());
        changed = expected.clone();
        changed.decision_digest = Some("selection-b".into());
        assert!(validate_exact_binding(&expected, &changed).is_err());
        changed = expected.clone();
        changed.health_digest = "health-b".into();
        assert!(validate_exact_binding(&expected, &changed).is_err());
        changed = expected.clone();
        changed.owner.as_mut().unwrap().compare_id += 1;
        assert!(validate_exact_binding(&expected, &changed).is_err());
    }

    #[test]
    fn registry_identity_is_rechecked_after_slow_probes_before_reservation() {
        let reviewed = loaded("photos", "revision-a", 1);
        assert!(validate_loaded_target_unchanged(&reviewed, &reviewed).is_ok());

        let renamed = loaded("archive", "revision-a", 1);
        assert!(validate_loaded_target_unchanged(&reviewed, &renamed).is_ok());
        let revised = loaded("photos", "revision-b", 1);
        assert!(validate_loaded_target_unchanged(&reviewed, &revised).is_err());
        let retargeted = loaded("photos", "revision-a", 0);
        assert!(validate_loaded_target_unchanged(&reviewed, &retargeted).is_err());
        let mut replaced = loaded("photos", "revision-a", 1);
        replaced.full_job.job_id = "job-b".into();
        assert!(validate_loaded_target_unchanged(&reviewed, &replaced).is_err());
    }
}
