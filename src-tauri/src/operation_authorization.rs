//! Typed, process-local review challenges, session grants, and one-use operation authority.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::autoscan::{AutoApplyTicket, AutoScanComparePermit};
use crate::compare_results::CompareScope;
use crate::dto::{CompareIdentity, SelectedRowDto};

const CHALLENGE_TTL: Duration = Duration::from_secs(10 * 60);
const AUTHORIZATION_TTL: Duration = Duration::from_secs(2 * 60);
const CHALLENGE_CAPACITY: usize = 32;
const AUTHORIZATION_CAPACITY: usize = 32;
const GRANT_CAPACITY: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JobTargetRevision {
    job_id: String,
    config_revision: String,
    target_index: usize,
}

impl JobTargetRevision {
    pub(crate) fn new(
        job_id: String,
        config_revision: String,
        target_index: usize,
    ) -> Result<Self, String> {
        if job_id.is_empty() || config_revision.is_empty() {
            return Err("The operation target identity is incomplete".into());
        }
        Ok(Self {
            job_id,
            config_revision,
            target_index,
        })
    }

    pub(crate) fn job_id(&self) -> &str {
        &self.job_id
    }

    pub(crate) fn config_revision(&self) -> &str {
        &self.config_revision
    }

    pub(crate) fn target_index(&self) -> usize {
        self.target_index
    }
}

impl From<&CompareIdentity> for JobTargetRevision {
    fn from(identity: &CompareIdentity) -> Self {
        Self {
            job_id: identity.job_id.clone(),
            config_revision: identity.config_revision.clone(),
            target_index: identity.target_index,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompareOrigin {
    Interactive,
    AutoScan(AutoScanComparePermit),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompareAuthorization {
    target: JobTargetRevision,
    capability_review_digest: String,
    origin: CompareOrigin,
}

impl CompareAuthorization {
    pub(crate) fn new(
        target: JobTargetRevision,
        capability_review_digest: String,
        origin: CompareOrigin,
    ) -> Result<Self, String> {
        if capability_review_digest.is_empty() {
            return Err("The Compare capability review is incomplete".into());
        }
        Ok(Self {
            target,
            capability_review_digest,
            origin,
        })
    }

    pub(crate) fn target(&self) -> &JobTargetRevision {
        &self.target
    }

    pub(crate) fn capability_review_digest(&self) -> &str {
        &self.capability_review_digest
    }

    pub(crate) fn auto_scan_permit(&self) -> Option<&AutoScanComparePermit> {
        match &self.origin {
            CompareOrigin::Interactive => None,
            CompareOrigin::AutoScan(permit) => Some(permit),
        }
    }

    pub(crate) fn origin(&self) -> &CompareOrigin {
        &self.origin
    }

    pub(crate) fn verify_current(&self, current: &Self) -> Result<(), String> {
        if self.target != current.target {
            return Err("The authorized job, revision, or target changed — review again".into());
        }
        if self.capability_review_digest != current.capability_review_digest {
            return Err("The backend capability report changed — review Compare again".into());
        }
        if self.origin != current.origin {
            return Err(
                "The Compare launch origin or AutoScan permit changed — review again".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExactSelection {
    selected_rows: Vec<SelectedRowDto>,
    selection_digest: String,
}

impl ExactSelection {
    pub(crate) fn new(selected_rows: Vec<SelectedRowDto>) -> Result<Self, String> {
        if selected_rows.is_empty() {
            return Err("An Apply authorization must bind at least one selected operation".into());
        }
        let selection_digest = selection_digest(&selected_rows)?;
        Ok(Self {
            selected_rows,
            selection_digest,
        })
    }

    pub(crate) fn rows(&self) -> &[SelectedRowDto] {
        &self.selected_rows
    }

    pub(crate) fn digest(&self) -> &str {
        &self.selection_digest
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ApplyReview {
    compare_identity: CompareIdentity,
    plan_digest: String,
    selection: ExactSelection,
    health_review_digest: String,
    capability_review_digest: String,
}

impl ApplyReview {
    pub(crate) fn new(
        compare_identity: CompareIdentity,
        plan_digest: String,
        selected_rows: Vec<SelectedRowDto>,
        health_review_digest: String,
        capability_review_digest: String,
    ) -> Result<Self, String> {
        if compare_identity.job_id.is_empty()
            || compare_identity.config_revision.is_empty()
            || plan_digest.is_empty()
            || health_review_digest.is_empty()
            || capability_review_digest.is_empty()
        {
            return Err("The Apply review fingerprint is incomplete".into());
        }
        Ok(Self {
            compare_identity,
            plan_digest,
            selection: ExactSelection::new(selected_rows)?,
            health_review_digest,
            capability_review_digest,
        })
    }

    pub(crate) fn target(&self) -> JobTargetRevision {
        JobTargetRevision::from(&self.compare_identity)
    }

    pub(crate) fn compare_identity(&self) -> &CompareIdentity {
        &self.compare_identity
    }

    pub(crate) fn selected_rows(&self) -> &[SelectedRowDto] {
        self.selection.rows()
    }

    pub(crate) fn capability_review_digest(&self) -> &str {
        &self.capability_review_digest
    }

    pub(crate) fn verify_current(&self, current: &Self) -> Result<(), String> {
        if self.compare_identity != current.compare_identity {
            return Err("The authorized Compare result changed — review Apply again".into());
        }
        if self.plan_digest != current.plan_digest {
            return Err("The authorized plan changed — review Apply again".into());
        }
        if self.selection.digest() != current.selection.digest() {
            return Err(
                "The authorized selected operation set changed — review Apply again".into(),
            );
        }
        if self.health_review_digest != current.health_review_digest {
            return Err("The plan health report changed — review Apply again".into());
        }
        if self.capability_review_digest != current.capability_review_digest {
            return Err("The backend capability report changed — review Apply again".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InteractiveApplyAuthorization {
    review: ApplyReview,
    health_warning_acknowledged: bool,
}

impl InteractiveApplyAuthorization {
    fn new(review: ApplyReview, health_warning_acknowledged: bool) -> Self {
        Self {
            review,
            health_warning_acknowledged,
        }
    }

    pub(crate) fn review(&self) -> &ApplyReview {
        &self.review
    }

    pub(crate) fn health_warning_acknowledged(&self) -> bool {
        self.health_warning_acknowledged
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AutoApplyAuthorization {
    review: ApplyReview,
    ticket: AutoApplyTicket,
}

impl AutoApplyAuthorization {
    fn new(review: ApplyReview, ticket: AutoApplyTicket) -> Result<Self, String> {
        if ticket.compare_identity() != &review.compare_identity {
            return Err("The AutoScan ticket does not match its Apply review".into());
        }
        Ok(Self { review, ticket })
    }

    pub(crate) fn review(&self) -> &ApplyReview {
        &self.review
    }

    pub(crate) fn ticket(&self) -> &AutoApplyTicket {
        &self.ticket
    }
}

#[derive(Clone, Debug)]
enum OperationAuthorization {
    Compare(CompareAuthorization),
    InteractiveApply(InteractiveApplyAuthorization),
    AutoApply(AutoApplyAuthorization),
}

#[derive(Clone, Debug)]
pub(crate) enum ApplyAuthorization {
    Interactive(InteractiveApplyAuthorization),
    AutoScan(AutoApplyAuthorization),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplyAuthorizationKind {
    Interactive,
    AutoScan,
}

impl ApplyAuthorization {
    pub(crate) fn kind(&self) -> ApplyAuthorizationKind {
        match self {
            Self::Interactive(_) => ApplyAuthorizationKind::Interactive,
            Self::AutoScan(_) => ApplyAuthorizationKind::AutoScan,
        }
    }

    pub(crate) fn review(&self) -> &ApplyReview {
        match self {
            Self::Interactive(authorization) => authorization.review(),
            Self::AutoScan(authorization) => authorization.review(),
        }
    }

    pub(crate) fn health_warning_acknowledged(&self) -> bool {
        match self {
            Self::Interactive(authorization) => authorization.health_warning_acknowledged(),
            Self::AutoScan(_) => false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ReviewChallenge {
    Compare {
        authorization: CompareAuthorization,
        requires_capability_ack: bool,
    },
    InteractiveApply {
        review: ApplyReview,
        requires_health_ack: bool,
        requires_capability_ack: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ReviewApproval {
    Compare {
        accept_capabilities: bool,
        remember_for_session: bool,
    },
    InteractiveApply {
        acknowledge_health: bool,
        accept_capabilities: bool,
        session_grant: ApplySessionGrantDecision,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplySessionGrantDecision {
    None,
    RememberCapabilities,
    AllowAutoApply,
}

#[derive(Clone, Debug)]
struct ChallengeRecord {
    id: String,
    challenge: ReviewChallenge,
    expires: Instant,
}

#[derive(Clone, Debug)]
struct AuthorizationRecord {
    token: String,
    authorization: OperationAuthorization,
    expires: Instant,
}

struct PreparedAuthorization {
    record: AuthorizationRecord,
    issued: IssuedAuthorization,
}

#[derive(Clone, Debug)]
pub(crate) struct IssuedChallenge {
    pub(crate) challenge_id: String,
    pub(crate) expires_at_ms: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct IssuedAuthorization {
    pub(crate) authorization_token: String,
    pub(crate) expires_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GrantScope {
    Compare,
    Apply,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GrantRecord {
    scope: GrantScope,
    target: JobTargetRevision,
    capability_review_digest: String,
    allow_auto_apply: bool,
}

impl GrantRecord {
    fn same_key(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.target == other.target
            && self.capability_review_digest == other.capability_review_digest
    }

    fn allows_compare(&self, authorization: &CompareAuthorization) -> bool {
        self.scope == GrantScope::Compare
            && self.target == authorization.target
            && self.capability_review_digest == authorization.capability_review_digest
    }

    fn allows_apply(&self, review: &ApplyReview, auto_apply: bool) -> bool {
        self.scope == GrantScope::Apply
            && self.target == review.target()
            && self.capability_review_digest == review.capability_review_digest
            && (!auto_apply || self.allow_auto_apply)
    }
}

#[derive(Default)]
struct AuthorizationState {
    challenges: VecDeque<ChallengeRecord>,
    authorizations: VecDeque<AuthorizationRecord>,
    grants: VecDeque<GrantRecord>,
}

#[derive(Default)]
pub(crate) struct OperationAuthorizationStore(Mutex<AuthorizationState>);

impl OperationAuthorizationStore {
    pub(crate) fn create_review_challenge(
        &self,
        challenge: ReviewChallenge,
    ) -> Result<IssuedChallenge, String> {
        self.create_review_challenge_at(challenge, Instant::now())
    }

    fn create_review_challenge_at(
        &self,
        challenge: ReviewChallenge,
        now: Instant,
    ) -> Result<IssuedChallenge, String> {
        let id = random_token()?;
        let mut state = self.0.lock().unwrap();
        purge(&mut state, now);
        state.challenges.push_back(ChallengeRecord {
            id: id.clone(),
            challenge,
            expires: now + CHALLENGE_TTL,
        });
        trim_front(&mut state.challenges, CHALLENGE_CAPACITY);
        Ok(IssuedChallenge {
            challenge_id: id,
            expires_at_ms: wall_expiry_ms(CHALLENGE_TTL),
        })
    }

    pub(crate) fn approve_review_challenge(
        &self,
        challenge_id: &str,
        approval: ReviewApproval,
    ) -> Result<IssuedAuthorization, String> {
        self.approve_review_challenge_at(challenge_id, approval, Instant::now())
    }

    fn approve_review_challenge_at(
        &self,
        challenge_id: &str,
        approval: ReviewApproval,
        now: Instant,
    ) -> Result<IssuedAuthorization, String> {
        self.approve_review_challenge_at_with_token(challenge_id, approval, now, random_token)
    }

    fn approve_review_challenge_at_with_token(
        &self,
        challenge_id: &str,
        approval: ReviewApproval,
        now: Instant,
        create_token: impl FnOnce() -> Result<String, String>,
    ) -> Result<IssuedAuthorization, String> {
        let mut state = self.0.lock().unwrap();
        let index = state
            .challenges
            .iter()
            .position(|challenge| challenge.id == challenge_id)
            .ok_or_else(|| "This review challenge expired or was already used".to_string())?;
        // Burn before expiry, approval-shape, or acknowledgement inspection.
        let challenge = state
            .challenges
            .remove(index)
            .expect("a located challenge must exist");
        if challenge.expires <= now {
            return Err("This review challenge expired — review again".into());
        }

        let (authorization, grant) = match (challenge.challenge, approval) {
            (
                ReviewChallenge::Compare {
                    authorization,
                    requires_capability_ack,
                },
                ReviewApproval::Compare {
                    accept_capabilities,
                    remember_for_session,
                },
            ) => {
                if requires_capability_ack && !accept_capabilities {
                    return Err("The reviewed capability limitations were not accepted".into());
                }
                if remember_for_session && !requires_capability_ack {
                    return Err(
                        "There is no reviewed Compare capability limitation to remember".into(),
                    );
                }
                let grant = remember_for_session.then(|| GrantRecord {
                    scope: GrantScope::Compare,
                    target: authorization.target.clone(),
                    capability_review_digest: authorization.capability_review_digest.clone(),
                    allow_auto_apply: false,
                });
                (OperationAuthorization::Compare(authorization), grant)
            }
            (
                ReviewChallenge::InteractiveApply {
                    review,
                    requires_health_ack,
                    requires_capability_ack,
                },
                ReviewApproval::InteractiveApply {
                    acknowledge_health,
                    accept_capabilities,
                    session_grant,
                },
            ) => {
                if requires_health_ack && !acknowledge_health {
                    return Err("The reviewed health warning was not acknowledged".into());
                }
                if requires_capability_ack && !accept_capabilities {
                    return Err("The reviewed capability limitations were not accepted".into());
                }
                let allow_auto_apply = session_grant == ApplySessionGrantDecision::AllowAutoApply;
                let grant =
                    (session_grant != ApplySessionGrantDecision::None).then(|| GrantRecord {
                        scope: GrantScope::Apply,
                        target: review.target(),
                        capability_review_digest: review.capability_review_digest.clone(),
                        allow_auto_apply,
                    });
                (
                    OperationAuthorization::InteractiveApply(InteractiveApplyAuthorization::new(
                        review,
                        acknowledge_health,
                    )),
                    grant,
                )
            }
            _ => return Err("This approval belongs to a different review operation".into()),
        };

        let prepared = prepare_authorization(authorization, now, create_token)?;

        if let Some(grant) = grant {
            if let Some(existing) = state
                .grants
                .iter()
                .position(|candidate| candidate.same_key(&grant))
            {
                state.grants.remove(existing);
            }
            if grant.scope == GrantScope::Apply && !grant.allow_auto_apply {
                state
                    .authorizations
                    .retain(|record| match &record.authorization {
                        OperationAuthorization::AutoApply(authorization) => {
                            !grant.allows_apply(authorization.review(), false)
                        }
                        _ => true,
                    });
            }
            state.grants.push_back(grant);
            trim_front(&mut state.grants, GRANT_CAPACITY);
        }

        Ok(commit_authorization(&mut state, prepared))
    }

    pub(crate) fn issue_compare_authorization(
        &self,
        authorization: CompareAuthorization,
    ) -> Result<IssuedAuthorization, String> {
        let mut state = self.0.lock().unwrap();
        let now = Instant::now();
        purge(&mut state, now);
        issue_into(
            &mut state,
            OperationAuthorization::Compare(authorization),
            now,
        )
    }

    pub(crate) fn issue_auto_apply_authorization(
        &self,
        review: ApplyReview,
        ticket: AutoApplyTicket,
    ) -> Result<IssuedAuthorization, String> {
        let authorization = AutoApplyAuthorization::new(review, ticket)?;
        let mut state = self.0.lock().unwrap();
        let now = Instant::now();
        purge(&mut state, now);
        let Some(index) = state
            .grants
            .iter()
            .position(|grant| grant.allows_apply(authorization.review(), true))
        else {
            return Err(
                "This AutoScan Apply has no exact session grant — review Apply interactively"
                    .into(),
            );
        };
        let prepared = prepare_authorization(
            OperationAuthorization::AutoApply(authorization),
            now,
            random_token,
        )?;
        touch_grant(&mut state.grants, index);
        Ok(commit_authorization(&mut state, prepared))
    }

    pub(crate) fn consume_compare_authorization(
        &self,
        token: &str,
    ) -> Result<CompareAuthorization, String> {
        match self.take_authorization(token, Instant::now())? {
            OperationAuthorization::Compare(authorization) => Ok(authorization),
            _ => Err("This operation authorization does not permit Compare".into()),
        }
    }

    pub(crate) fn consume_apply_authorization(
        &self,
        token: &str,
    ) -> Result<ApplyAuthorization, String> {
        match self.take_authorization(token, Instant::now())? {
            OperationAuthorization::InteractiveApply(authorization) => {
                Ok(ApplyAuthorization::Interactive(authorization))
            }
            OperationAuthorization::AutoApply(authorization) => {
                Ok(ApplyAuthorization::AutoScan(authorization))
            }
            OperationAuthorization::Compare(_) => {
                Err("This operation authorization does not permit Apply".into())
            }
        }
    }

    fn take_authorization(
        &self,
        token: &str,
        now: Instant,
    ) -> Result<OperationAuthorization, String> {
        let mut state = self.0.lock().unwrap();
        let index = state
            .authorizations
            .iter()
            .position(|authorization| authorization.token == token)
            .ok_or_else(|| {
                "This operation authorization is invalid, expired, or already used".to_string()
            })?;
        // Burn before expiry or variant inspection, so a wrong command cannot probe and replay it.
        let record = state
            .authorizations
            .remove(index)
            .expect("a located authorization must exist");
        if record.expires <= now {
            return Err("This operation authorization expired — review again".into());
        }
        Ok(record.authorization)
    }

    pub(crate) fn has_compare_capability_grant(
        &self,
        authorization: &CompareAuthorization,
    ) -> bool {
        self.find_grant(|grant| grant.allows_compare(authorization))
    }

    pub(crate) fn has_interactive_apply_capability_grant(&self, review: &ApplyReview) -> bool {
        self.find_grant(|grant| grant.allows_apply(review, false))
    }

    fn find_grant(&self, predicate: impl Fn(&GrantRecord) -> bool) -> bool {
        let mut state = self.0.lock().unwrap();
        let Some(index) = state.grants.iter().position(predicate) else {
            return false;
        };
        touch_grant(&mut state.grants, index);
        true
    }

    pub(crate) fn revoke_job_authority(&self, job_id: &str) {
        let mut state = self.0.lock().unwrap();
        state.challenges.retain(|record| match &record.challenge {
            ReviewChallenge::Compare { authorization, .. } => authorization.target.job_id != job_id,
            ReviewChallenge::InteractiveApply { review, .. } => {
                review.compare_identity.job_id != job_id
            }
        });
        state
            .authorizations
            .retain(|record| match &record.authorization {
                OperationAuthorization::Compare(authorization) => {
                    authorization.target.job_id != job_id
                }
                OperationAuthorization::InteractiveApply(authorization) => {
                    authorization.review.compare_identity.job_id != job_id
                }
                OperationAuthorization::AutoApply(authorization) => {
                    authorization.review.compare_identity.job_id != job_id
                }
            });
        state.grants.retain(|record| record.target.job_id != job_id);
    }

    pub(crate) fn revoke_apply_authority(&self, scope: &CompareScope) {
        let mut state = self.0.lock().unwrap();
        state.challenges.retain(|record| match &record.challenge {
            ReviewChallenge::Compare { .. } => true,
            ReviewChallenge::InteractiveApply { review, .. } => {
                !scope.contains(review.compare_identity())
            }
        });
        state
            .authorizations
            .retain(|record| match &record.authorization {
                OperationAuthorization::Compare(_) => true,
                OperationAuthorization::InteractiveApply(authorization) => {
                    !scope.contains(authorization.review().compare_identity())
                }
                OperationAuthorization::AutoApply(authorization) => {
                    !scope.contains(authorization.review().compare_identity())
                }
            });
        // Session grants record reviewed capability consent, not evidence freshness. Keeping them
        // lets a successful new Compare restore unattended operation without weakening the result gate.
    }
}

fn issue_into(
    state: &mut AuthorizationState,
    authorization: OperationAuthorization,
    now: Instant,
) -> Result<IssuedAuthorization, String> {
    let prepared = prepare_authorization(authorization, now, random_token)?;
    Ok(commit_authorization(state, prepared))
}

fn prepare_authorization(
    authorization: OperationAuthorization,
    now: Instant,
    create_token: impl FnOnce() -> Result<String, String>,
) -> Result<PreparedAuthorization, String> {
    let token = create_token()?;
    Ok(PreparedAuthorization {
        record: AuthorizationRecord {
            token: token.clone(),
            authorization,
            expires: now + AUTHORIZATION_TTL,
        },
        issued: IssuedAuthorization {
            authorization_token: token,
            expires_at_ms: wall_expiry_ms(AUTHORIZATION_TTL),
        },
    })
}

fn commit_authorization(
    state: &mut AuthorizationState,
    prepared: PreparedAuthorization,
) -> IssuedAuthorization {
    state.authorizations.push_back(prepared.record);
    trim_front(&mut state.authorizations, AUTHORIZATION_CAPACITY);
    prepared.issued
}

fn purge(state: &mut AuthorizationState, now: Instant) {
    state.challenges.retain(|record| record.expires > now);
    state.authorizations.retain(|record| record.expires > now);
}

fn trim_front<T>(records: &mut VecDeque<T>, capacity: usize) {
    while records.len() > capacity {
        records.pop_front();
    }
}

fn touch_grant(grants: &mut VecDeque<GrantRecord>, index: usize) {
    if index + 1 != grants.len() {
        let grant = grants.remove(index).expect("a located grant must exist");
        grants.push_back(grant);
    }
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("Cannot create an operation authorization: {error}"))?;
    let mut token = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(token)
}

fn wall_expiry_ms(ttl: Duration) -> u64 {
    syncdash::foundation::time::now_ms().saturating_add(ttl.as_millis() as u64)
}

pub(crate) fn selection_digest(selected: &[SelectedRowDto]) -> Result<String, String> {
    let mut normalized: Vec<(usize, bool)> = selected
        .iter()
        .map(|decision| (decision.index, decision.flipped))
        .collect();
    normalized.sort_unstable();
    if normalized.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("The selected action set contains a duplicate row".into());
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"syncdash-selected-decisions-v1\0");
    for (index, flipped) in normalized {
        hasher.update(&(index as u64).to_le_bytes());
        hasher.update(&[u8::from(flipped)]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(crate) fn health_review_digest(
    unacknowledged: &syncdash::pipeline::guard::Verdict,
    acknowledged: &syncdash::pipeline::guard::Verdict,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"syncdash-health-review-v1\0");
    hash_messages(&mut hasher, b"unack-blockers", &unacknowledged.blockers);
    hash_messages(&mut hasher, b"unack-warnings", &unacknowledged.warnings);
    hash_messages(&mut hasher, b"ack-blockers", &acknowledged.blockers);
    hash_messages(&mut hasher, b"ack-warnings", &acknowledged.warnings);
    hasher.finalize().to_hex().to_string()
}

fn hash_messages(hasher: &mut blake3::Hasher, label: &[u8], messages: &[String]) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label);
    let mut normalized = messages.to_vec();
    normalized.sort();
    for message in normalized {
        hasher.update(&(message.len() as u64).to_le_bytes());
        hasher.update(message.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    fn compare_authorization() -> CompareAuthorization {
        CompareAuthorization::new(
            JobTargetRevision::new("job-a".into(), "revision-a".into(), 1).unwrap(),
            "caps-a".into(),
            CompareOrigin::Interactive,
        )
        .unwrap()
    }

    fn identity() -> CompareIdentity {
        CompareIdentity {
            compare_run_id: 9,
            job_id: "job-a".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
        }
    }

    fn apply_review() -> ApplyReview {
        ApplyReview::new(
            identity(),
            "plan-a".into(),
            vec![SelectedRowDto {
                index: 3,
                flipped: true,
            }],
            "health-a".into(),
            "caps-a".into(),
        )
        .unwrap()
    }

    #[test]
    fn selection_and_digest_are_created_atomically() {
        assert!(ExactSelection::new(Vec::new()).is_err());
        assert!(ExactSelection::new(vec![
            SelectedRowDto {
                index: 2,
                flipped: false,
            },
            SelectedRowDto {
                index: 2,
                flipped: true,
            },
        ])
        .is_err());
        let a = selection_digest(&[
            SelectedRowDto {
                index: 2,
                flipped: false,
            },
            SelectedRowDto {
                index: 1,
                flipped: true,
            },
        ])
        .unwrap();
        let b = selection_digest(&[
            SelectedRowDto {
                index: 1,
                flipped: true,
            },
            SelectedRowDto {
                index: 2,
                flipped: false,
            },
        ])
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn apply_review_verifies_the_stable_compare_identity() {
        let expected = apply_review();
        let current = ApplyReview::new(
            identity(),
            "plan-a".into(),
            expected.selected_rows().to_vec(),
            "health-a".into(),
            "caps-a".into(),
        )
        .unwrap();
        assert!(expected.verify_current(&current).is_ok());
    }

    fn approve_apply_grant(
        store: &OperationAuthorizationStore,
        review: ApplyReview,
        session_grant: ApplySessionGrantDecision,
    ) -> IssuedAuthorization {
        let challenge = store
            .create_review_challenge(ReviewChallenge::InteractiveApply {
                review,
                requires_health_ack: false,
                requires_capability_ack: true,
            })
            .unwrap();
        store
            .approve_review_challenge(
                &challenge.challenge_id,
                ReviewApproval::InteractiveApply {
                    acknowledge_health: false,
                    accept_capabilities: true,
                    session_grant,
                },
            )
            .unwrap()
    }

    #[test]
    fn wrong_consumer_burns_token_and_parallel_consume_has_one_winner() {
        let store = Arc::new(OperationAuthorizationStore::default());
        let issued = store
            .issue_compare_authorization(compare_authorization())
            .unwrap();
        assert!(store
            .consume_apply_authorization(&issued.authorization_token)
            .is_err());
        assert!(store
            .consume_compare_authorization(&issued.authorization_token)
            .is_err());

        let issued = store
            .issue_compare_authorization(compare_authorization())
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let store = store.clone();
            let barrier = barrier.clone();
            let token = issued.authorization_token.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                store.consume_compare_authorization(&token).is_ok()
            }));
        }
        barrier.wait();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
    }

    #[test]
    fn wrong_approval_variant_burns_challenge() {
        let store = OperationAuthorizationStore::default();
        let challenge = store
            .create_review_challenge(ReviewChallenge::Compare {
                authorization: compare_authorization(),
                requires_capability_ack: true,
            })
            .unwrap();
        let wrong = ReviewApproval::InteractiveApply {
            acknowledge_health: false,
            accept_capabilities: true,
            session_grant: ApplySessionGrantDecision::None,
        };
        assert!(store
            .approve_review_challenge(&challenge.challenge_id, wrong)
            .is_err());
        assert!(store
            .approve_review_challenge(
                &challenge.challenge_id,
                ReviewApproval::Compare {
                    accept_capabilities: true,
                    remember_for_session: false,
                },
            )
            .is_err());
    }

    #[test]
    fn failed_token_creation_cannot_commit_a_session_grant() {
        let store = OperationAuthorizationStore::default();
        let challenge = store
            .create_review_challenge(ReviewChallenge::InteractiveApply {
                review: apply_review(),
                requires_health_ack: false,
                requires_capability_ack: true,
            })
            .unwrap();

        let result = store.approve_review_challenge_at_with_token(
            &challenge.challenge_id,
            ReviewApproval::InteractiveApply {
                acknowledge_health: false,
                accept_capabilities: true,
                session_grant: ApplySessionGrantDecision::AllowAutoApply,
            },
            Instant::now(),
            || Err("entropy source unavailable".into()),
        );

        assert!(result.is_err());
        let state = store.0.lock().unwrap();
        assert!(state.challenges.is_empty());
        assert!(state.authorizations.is_empty());
        assert!(state.grants.is_empty());
    }

    #[test]
    fn expired_authorization_is_burned_before_it_is_reported() {
        let store = OperationAuthorizationStore::default();
        let issued = store
            .issue_compare_authorization(compare_authorization())
            .unwrap();
        store
            .0
            .lock()
            .unwrap()
            .authorizations
            .front_mut()
            .unwrap()
            .expires = Instant::now() - Duration::from_secs(1);

        assert!(store
            .consume_compare_authorization(&issued.authorization_token)
            .unwrap_err()
            .contains("expired"));
        assert!(store
            .consume_compare_authorization(&issued.authorization_token)
            .is_err());
    }

    #[test]
    fn grants_cannot_cross_scope_revision_target_or_capability_digest() {
        let store = OperationAuthorizationStore::default();
        let compare = compare_authorization();
        let challenge = store
            .create_review_challenge(ReviewChallenge::Compare {
                authorization: compare.clone(),
                requires_capability_ack: true,
            })
            .unwrap();
        store
            .approve_review_challenge(
                &challenge.challenge_id,
                ReviewApproval::Compare {
                    accept_capabilities: true,
                    remember_for_session: true,
                },
            )
            .unwrap();
        assert!(store.has_compare_capability_grant(&compare));

        let revised = CompareAuthorization::new(
            JobTargetRevision::new("job-a".into(), "revision-b".into(), 1).unwrap(),
            "caps-a".into(),
            CompareOrigin::Interactive,
        )
        .unwrap();
        let retargeted = CompareAuthorization::new(
            JobTargetRevision::new("job-a".into(), "revision-a".into(), 0).unwrap(),
            "caps-a".into(),
            CompareOrigin::Interactive,
        )
        .unwrap();
        let recapped = CompareAuthorization::new(
            JobTargetRevision::new("job-a".into(), "revision-a".into(), 1).unwrap(),
            "caps-b".into(),
            CompareOrigin::Interactive,
        )
        .unwrap();
        assert!(!store.has_compare_capability_grant(&revised));
        assert!(!store.has_compare_capability_grant(&retargeted));
        assert!(!store.has_compare_capability_grant(&recapped));
        assert!(!store.has_interactive_apply_capability_grant(&apply_review()));
    }

    #[test]
    fn auto_apply_requires_an_exact_grant_and_downgrade_revokes_issued_tokens() {
        let store = OperationAuthorizationStore::default();
        let review = apply_review();
        let ticket = AutoApplyTicket::for_test(4, 12, identity());
        assert!(store
            .issue_auto_apply_authorization(review.clone(), ticket.clone())
            .is_err());

        approve_apply_grant(
            &store,
            review.clone(),
            ApplySessionGrantDecision::AllowAutoApply,
        );
        let issued = store
            .issue_auto_apply_authorization(review.clone(), ticket)
            .unwrap();

        approve_apply_grant(
            &store,
            review,
            ApplySessionGrantDecision::RememberCapabilities,
        );
        assert!(store
            .consume_apply_authorization(&issued.authorization_token)
            .is_err());
    }

    #[test]
    fn dirty_scope_revokes_apply_challenges_and_tokens_but_retains_session_consent() {
        let store = OperationAuthorizationStore::default();
        let review = apply_review();
        let pending = store
            .create_review_challenge(ReviewChallenge::InteractiveApply {
                review: review.clone(),
                requires_health_ack: false,
                requires_capability_ack: false,
            })
            .unwrap();
        let interactive = approve_apply_grant(
            &store,
            review.clone(),
            ApplySessionGrantDecision::AllowAutoApply,
        );
        let automatic = store
            .issue_auto_apply_authorization(
                review.clone(),
                AutoApplyTicket::for_test(4, 12, identity()),
            )
            .unwrap();

        store.revoke_apply_authority(&CompareScope::new("job-a", 1, "revision-a"));

        assert!(store
            .approve_review_challenge(
                &pending.challenge_id,
                ReviewApproval::InteractiveApply {
                    acknowledge_health: false,
                    accept_capabilities: false,
                    session_grant: ApplySessionGrantDecision::None,
                },
            )
            .is_err());
        assert!(store
            .consume_apply_authorization(&interactive.authorization_token)
            .is_err());
        assert!(store
            .consume_apply_authorization(&automatic.authorization_token)
            .is_err());
        assert!(store.has_interactive_apply_capability_grant(&review));
        assert!(store.find_grant(|grant| grant.allows_apply(&review, true)));
    }

    #[test]
    fn every_apply_fingerprint_and_health_message_set_is_exact() {
        let expected = apply_review();
        let mut changed_identity = identity();
        changed_identity.compare_run_id += 1;
        let changed = ApplyReview::new(
            changed_identity,
            "plan-a".into(),
            expected.selected_rows().to_vec(),
            "health-a".into(),
            "caps-a".into(),
        )
        .unwrap();
        assert!(expected.verify_current(&changed).is_err());

        let first = syncdash::pipeline::guard::Verdict {
            blockers: vec!["b".into(), "a".into()],
            warnings: vec!["w".into()],
        };
        let reordered = syncdash::pipeline::guard::Verdict {
            blockers: vec!["a".into(), "b".into()],
            warnings: vec!["w".into()],
        };
        let acknowledged = syncdash::pipeline::guard::Verdict {
            blockers: Vec::new(),
            warnings: vec!["w".into()],
        };
        assert_eq!(
            health_review_digest(&first, &acknowledged),
            health_review_digest(&reordered, &acknowledged)
        );
        assert_ne!(
            health_review_digest(&first, &acknowledged),
            health_review_digest(&acknowledged, &first)
        );
    }

    #[test]
    fn authorization_capacity_and_job_revocation_remain_exact() {
        let store = OperationAuthorizationStore::default();
        for index in 0..(AUTHORIZATION_CAPACITY + 5) {
            let job_id = if index % 2 == 0 { "job-a" } else { "job-b" };
            store
                .issue_compare_authorization(
                    CompareAuthorization::new(
                        JobTargetRevision::new(job_id.into(), "revision-a".into(), 0).unwrap(),
                        format!("caps-{index}"),
                        CompareOrigin::Interactive,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(
            store.0.lock().unwrap().authorizations.len(),
            AUTHORIZATION_CAPACITY
        );
        store.revoke_job_authority("job-a");
        let state = store.0.lock().unwrap();
        assert!(state
            .authorizations
            .iter()
            .all(|record| match &record.authorization {
                OperationAuthorization::Compare(authorization) => {
                    authorization.target.job_id() == "job-b"
                }
                _ => false,
            }));
        assert!(state.authorizations.iter().any(|_| true));
    }

    #[test]
    fn remembered_apply_capability_consent_is_reused_without_auto_apply_permission() {
        let store = OperationAuthorizationStore::default();
        let review = apply_review();
        let challenge = store
            .create_review_challenge(ReviewChallenge::InteractiveApply {
                review: review.clone(),
                requires_health_ack: false,
                requires_capability_ack: true,
            })
            .unwrap();
        store
            .approve_review_challenge(
                &challenge.challenge_id,
                ReviewApproval::InteractiveApply {
                    acknowledge_health: false,
                    accept_capabilities: true,
                    session_grant: ApplySessionGrantDecision::RememberCapabilities,
                },
            )
            .unwrap();
        assert!(store.has_interactive_apply_capability_grant(&review));
        assert!(!store.find_grant(|grant| grant.allows_apply(&review, true)));
    }
}
