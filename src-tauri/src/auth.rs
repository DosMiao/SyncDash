//! Process-local, bounded review challenges, session grants, and one-use operation tokens.
//!
//! Nothing in this store is authority by itself: command handlers reload the job and compare
//! repository entry after consuming a token, then recompute every fingerprint immediately before
//! reserving a run. The store makes approval non-forgeable, scoped, expiring, and non-replayable.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use syncdash::pipeline::guard::caps::CapabilityScope;

use crate::dto::{CompareOwner, SelectedRowDto};

const CHALLENGE_TTL: Duration = Duration::from_secs(10 * 60);
const AUTHORIZATION_TTL: Duration = Duration::from_secs(2 * 60);
const CHALLENGE_CAPACITY: usize = 32;
const AUTHORIZATION_CAPACITY: usize = 32;
const GRANT_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthorizationPurpose {
    CompareInteractive,
    ApplyInteractive,
    ApplyUnattended,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationBinding {
    pub(crate) scope: CapabilityScope,
    pub(crate) purpose: AuthorizationPurpose,
    pub(crate) job_id: String,
    pub(crate) job_name: String,
    pub(crate) config_revision: String,
    pub(crate) target_index: usize,
    pub(crate) owner: Option<CompareOwner>,
    pub(crate) plan_digest: Option<String>,
    pub(crate) decision_digest: Option<String>,
    pub(crate) health_digest: String,
    pub(crate) capability_digest: String,
}

impl OperationBinding {
    fn validate_shape(&self) -> Result<(), String> {
        if self.job_id.is_empty()
            || self.job_name.is_empty()
            || self.config_revision.is_empty()
            || self.health_digest.is_empty()
            || self.capability_digest.is_empty()
        {
            return Err("The operation authorization binding is incomplete".into());
        }

        match self.purpose {
            AuthorizationPurpose::CompareInteractive => {
                if self.scope != CapabilityScope::CompareRead {
                    return Err(
                        "A Compare authorization must use compare-read capability scope".into(),
                    );
                }
                if self.owner.is_some()
                    || self.plan_digest.is_some()
                    || self.decision_digest.is_some()
                {
                    return Err(
                        "A Compare authorization cannot carry an Apply plan or selection".into(),
                    );
                }
            }
            AuthorizationPurpose::ApplyInteractive | AuthorizationPurpose::ApplyUnattended => {
                if self.scope != CapabilityScope::ApplyWrite {
                    return Err(
                        "An Apply authorization must use apply-write capability scope".into(),
                    );
                }
                let owner = self.owner.as_ref().ok_or_else(|| {
                    "An Apply authorization must identify its Compare result".to_string()
                })?;
                if owner.job_id != self.job_id
                    || owner.job_name != self.job_name
                    || owner.config_revision != self.config_revision
                    || owner.target_index != self.target_index
                {
                    return Err("The Apply authorization does not match its Compare owner".into());
                }
                if self.plan_digest.as_deref().is_none_or(str::is_empty) {
                    return Err("An Apply authorization must bind an exact plan".into());
                }
                if self.decision_digest.as_deref().is_none_or(str::is_empty) {
                    return Err("An Apply authorization must bind an exact operation set".into());
                }
            }
        }
        Ok(())
    }

    fn validate(&self, selected: &[SelectedRowDto]) -> Result<(), String> {
        self.validate_shape()?;
        match self.purpose {
            AuthorizationPurpose::CompareInteractive => {
                if !selected.is_empty() {
                    return Err("A Compare authorization cannot carry selected operations".into());
                }
            }
            AuthorizationPurpose::ApplyInteractive | AuthorizationPurpose::ApplyUnattended => {
                if selected.is_empty() {
                    return Err(
                        "An Apply authorization must bind at least one selected operation".into(),
                    );
                }
                let expected = decision_digest(selected)?;
                if self.decision_digest.as_deref() != Some(expected.as_str()) {
                    return Err(
                        "The Apply authorization does not match its selected operation set".into(),
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChallengeSpec {
    pub(crate) binding: OperationBinding,
    pub(crate) selected: Vec<SelectedRowDto>,
    pub(crate) requires_health_ack: bool,
    pub(crate) requires_capability_ack: bool,
}

#[derive(Clone, Debug)]
struct ChallengeRecord {
    id: String,
    spec: ChallengeSpec,
    expires: Instant,
}

#[derive(Clone, Debug)]
pub(crate) struct AuthorizationRecord {
    pub(crate) token: String,
    pub(crate) binding: OperationBinding,
    pub(crate) selected: Vec<SelectedRowDto>,
    pub(crate) acknowledged_health: bool,
    expires: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GrantRecord {
    scope: CapabilityScope,
    job_id: String,
    config_revision: String,
    target_index: usize,
    capability_digest: String,
    allow_unattended: bool,
}

impl GrantRecord {
    fn same_key(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.job_id == other.job_id
            && self.config_revision == other.config_revision
            && self.target_index == other.target_index
            && self.capability_digest == other.capability_digest
    }

    fn allows(&self, binding: &OperationBinding, unattended: bool) -> bool {
        self.scope == binding.scope
            && self.job_id == binding.job_id
            && self.config_revision == binding.config_revision
            && self.target_index == binding.target_index
            && self.capability_digest == binding.capability_digest
            && (!unattended || self.allow_unattended)
    }
}

#[derive(Default)]
struct AuthorizationState {
    challenges: VecDeque<ChallengeRecord>,
    authorizations: VecDeque<AuthorizationRecord>,
    grants: VecDeque<GrantRecord>,
}

#[derive(Default)]
pub(crate) struct AuthorizationStore(Mutex<AuthorizationState>);

impl AuthorizationStore {
    pub(crate) fn challenge(&self, spec: ChallengeSpec) -> Result<(String, u64), String> {
        self.challenge_at(spec, Instant::now())
    }

    fn challenge_at(&self, spec: ChallengeSpec, now: Instant) -> Result<(String, u64), String> {
        if spec.binding.purpose == AuthorizationPurpose::ApplyUnattended {
            return Err(
                "An unattended Apply cannot create a review challenge; it requires an exact session grant"
                    .into(),
            );
        }
        spec.binding.validate(&spec.selected)?;
        let id = random_token()?;
        let mut state = self.0.lock().unwrap();
        purge(&mut state, now);
        state.challenges.push_back(ChallengeRecord {
            id: id.clone(),
            spec,
            expires: now + CHALLENGE_TTL,
        });
        trim_front(&mut state.challenges, CHALLENGE_CAPACITY);
        Ok((id, wall_expiry_ms(CHALLENGE_TTL)))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn approve(
        &self,
        challenge_id: &str,
        acknowledge_health: bool,
        accept_capabilities: bool,
        remember_for_session: bool,
        allow_unattended: bool,
    ) -> Result<(AuthorizationRecord, u64), String> {
        self.approve_at(
            challenge_id,
            acknowledge_health,
            accept_capabilities,
            remember_for_session,
            allow_unattended,
            Instant::now(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn approve_at(
        &self,
        challenge_id: &str,
        acknowledge_health: bool,
        accept_capabilities: bool,
        remember_for_session: bool,
        allow_unattended: bool,
        now: Instant,
    ) -> Result<(AuthorizationRecord, u64), String> {
        let mut state = self.0.lock().unwrap();
        purge(&mut state, now);
        let index = state
            .challenges
            .iter()
            .position(|challenge| challenge.id == challenge_id)
            .ok_or_else(|| "This review challenge expired or was already used".to_string())?;
        let challenge = state
            .challenges
            .remove(index)
            .expect("a located challenge must exist");
        if challenge.spec.requires_health_ack && !acknowledge_health {
            return Err("The reviewed health warning was not acknowledged".into());
        }
        if challenge.spec.requires_capability_ack && !accept_capabilities {
            return Err("The reviewed capability limitations were not accepted".into());
        }
        if allow_unattended
            && challenge.spec.binding.purpose != AuthorizationPurpose::ApplyInteractive
        {
            return Err(
                "Unattended permission can only be granted from an interactive Apply review".into(),
            );
        }
        if allow_unattended && !remember_for_session {
            return Err("Unattended permission requires an explicit session grant".into());
        }
        if remember_for_session
            && !challenge.spec.requires_capability_ack
            && challenge.spec.binding.purpose != AuthorizationPurpose::ApplyInteractive
        {
            return Err("There is no reviewed Compare capability limitation to remember".into());
        }

        let authorization = issue_record(
            challenge.spec.binding.clone(),
            challenge.spec.selected,
            acknowledge_health,
            now,
        )?;

        if remember_for_session {
            let binding = &challenge.spec.binding;
            let grant = GrantRecord {
                scope: binding.scope,
                job_id: binding.job_id.clone(),
                config_revision: binding.config_revision.clone(),
                target_index: binding.target_index,
                capability_digest: binding.capability_digest.clone(),
                allow_unattended,
            };
            if let Some(existing) = state
                .grants
                .iter()
                .position(|candidate| candidate.same_key(&grant))
            {
                state.grants.remove(existing);
            }
            if !grant.allow_unattended {
                state.authorizations.retain(|authorization| {
                    authorization.binding.purpose != AuthorizationPurpose::ApplyUnattended
                        || !grant.allows(&authorization.binding, false)
                });
            }
            state.grants.push_back(grant);
            trim_front(&mut state.grants, GRANT_CAPACITY);
        }

        state.authorizations.push_back(authorization.clone());
        trim_front(&mut state.authorizations, AUTHORIZATION_CAPACITY);
        Ok((authorization, wall_expiry_ms(AUTHORIZATION_TTL)))
    }

    pub(crate) fn authorize_direct(
        &self,
        binding: OperationBinding,
        selected: Vec<SelectedRowDto>,
        acknowledged_health: bool,
    ) -> Result<(AuthorizationRecord, u64), String> {
        self.authorize_direct_at(binding, selected, acknowledged_health, Instant::now())
    }

    fn authorize_direct_at(
        &self,
        binding: OperationBinding,
        selected: Vec<SelectedRowDto>,
        acknowledged_health: bool,
        now: Instant,
    ) -> Result<(AuthorizationRecord, u64), String> {
        if binding.purpose != AuthorizationPurpose::CompareInteractive {
            return Err(
                "Only Compare can be authorized directly; Apply requires a review challenge or an exact unattended session grant"
                    .into(),
            );
        }
        let authorization = issue_record(binding, selected, acknowledged_health, now)?;
        let mut state = self.0.lock().unwrap();
        purge(&mut state, now);
        state.authorizations.push_back(authorization.clone());
        trim_front(&mut state.authorizations, AUTHORIZATION_CAPACITY);
        Ok((authorization, wall_expiry_ms(AUTHORIZATION_TTL)))
    }

    pub(crate) fn authorize_unattended(
        &self,
        binding: OperationBinding,
        selected: Vec<SelectedRowDto>,
    ) -> Result<(AuthorizationRecord, u64), String> {
        self.authorize_unattended_at(binding, selected, Instant::now())
    }

    fn authorize_unattended_at(
        &self,
        binding: OperationBinding,
        selected: Vec<SelectedRowDto>,
        now: Instant,
    ) -> Result<(AuthorizationRecord, u64), String> {
        if binding.purpose != AuthorizationPurpose::ApplyUnattended {
            return Err("Only an unattended Apply can use an unattended session grant".into());
        }
        let authorization = issue_record(binding, selected, false, now)?;
        let mut state = self.0.lock().unwrap();
        purge(&mut state, now);
        let Some(index) = state
            .grants
            .iter()
            .position(|grant| grant.allows(&authorization.binding, true))
        else {
            return Err(
                "This unattended Apply has no exact session grant — review Apply interactively"
                    .into(),
            );
        };
        touch_grant(&mut state.grants, index);
        state.authorizations.push_back(authorization.clone());
        trim_front(&mut state.authorizations, AUTHORIZATION_CAPACITY);
        Ok((authorization, wall_expiry_ms(AUTHORIZATION_TTL)))
    }

    pub(crate) fn consume(
        &self,
        token: &str,
        purpose: AuthorizationPurpose,
    ) -> Result<AuthorizationRecord, String> {
        self.consume_at(token, purpose, Instant::now())
    }

    pub(crate) fn consume_apply(&self, token: &str) -> Result<AuthorizationRecord, String> {
        self.consume_apply_at(token, Instant::now())
    }

    fn consume_apply_at(&self, token: &str, now: Instant) -> Result<AuthorizationRecord, String> {
        let authorization = self.take_authorization(token, now)?;
        if !matches!(
            authorization.binding.purpose,
            AuthorizationPurpose::ApplyInteractive | AuthorizationPurpose::ApplyUnattended
        ) {
            return Err("This operation authorization does not permit Apply".into());
        }
        Ok(authorization)
    }

    fn consume_at(
        &self,
        token: &str,
        purpose: AuthorizationPurpose,
        now: Instant,
    ) -> Result<AuthorizationRecord, String> {
        let authorization = self.take_authorization(token, now)?;
        if authorization.binding.purpose != purpose {
            return Err("This operation authorization belongs to a different purpose".into());
        }
        Ok(authorization)
    }

    fn take_authorization(&self, token: &str, now: Instant) -> Result<AuthorizationRecord, String> {
        let mut state = self.0.lock().unwrap();
        let index = state
            .authorizations
            .iter()
            .position(|authorization| authorization.token == token)
            .ok_or_else(|| {
                "This operation authorization is invalid, expired, or already used".to_string()
            })?;
        // Remove before inspecting any binding. A guessed purpose or duplicated concurrent invoke
        // cannot probe and then replay a valid token.
        let authorization = state
            .authorizations
            .remove(index)
            .expect("a located authorization must exist");
        if authorization.expires <= now {
            return Err("This operation authorization expired — review again".into());
        }
        Ok(authorization)
    }

    pub(crate) fn grant_allows(&self, binding: &OperationBinding, unattended: bool) -> bool {
        if binding.validate_shape().is_err()
            || (unattended && binding.purpose != AuthorizationPurpose::ApplyUnattended)
        {
            return false;
        }
        let mut state = self.0.lock().unwrap();
        let Some(index) = state
            .grants
            .iter()
            .position(|grant| grant.allows(binding, unattended))
        else {
            return false;
        };
        touch_grant(&mut state.grants, index);
        true
    }

    pub(crate) fn revoke_job(&self, job_id: &str) {
        let mut state = self.0.lock().unwrap();
        state
            .challenges
            .retain(|record| record.spec.binding.job_id != job_id);
        state
            .authorizations
            .retain(|record| record.binding.job_id != job_id);
        state.grants.retain(|record| record.job_id != job_id);
    }
}

fn issue_record(
    binding: OperationBinding,
    selected: Vec<SelectedRowDto>,
    acknowledged_health: bool,
    now: Instant,
) -> Result<AuthorizationRecord, String> {
    binding.validate(&selected)?;
    Ok(AuthorizationRecord {
        token: random_token()?,
        binding,
        selected,
        acknowledged_health,
        expires: now + AUTHORIZATION_TTL,
    })
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

pub(crate) fn decision_digest(selected: &[SelectedRowDto]) -> Result<String, String> {
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

pub(crate) fn health_digest(
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
    let mut messages = messages.to_vec();
    messages.sort();
    for message in messages {
        hasher.update(&(message.len() as u64).to_le_bytes());
        hasher.update(message.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    fn owner() -> CompareOwner {
        CompareOwner {
            compare_id: 9,
            job_id: "job-a".into(),
            job_name: "photos".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
        }
    }

    fn selection() -> Vec<SelectedRowDto> {
        vec![SelectedRowDto {
            index: 3,
            flipped: true,
        }]
    }

    fn binding(purpose: AuthorizationPurpose) -> OperationBinding {
        let applies = matches!(
            purpose,
            AuthorizationPurpose::ApplyInteractive | AuthorizationPurpose::ApplyUnattended
        );
        OperationBinding {
            scope: if applies {
                CapabilityScope::ApplyWrite
            } else {
                CapabilityScope::CompareRead
            },
            purpose,
            job_id: "job-a".into(),
            job_name: "photos".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
            owner: applies.then(owner),
            plan_digest: applies.then(|| "plan-a".into()),
            decision_digest: applies.then(|| decision_digest(&selection()).unwrap()),
            health_digest: "health-a".into(),
            capability_digest: "caps-a".into(),
        }
    }

    #[test]
    fn authorization_is_one_use_even_under_parallel_consumption() {
        let store = Arc::new(AuthorizationStore::default());
        let (authorization, _) = store
            .authorize_direct(
                binding(AuthorizationPurpose::CompareInteractive),
                Vec::new(),
                false,
            )
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let store = store.clone();
            let barrier = barrier.clone();
            let token = authorization.token.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                store
                    .consume(&token, AuthorizationPurpose::CompareInteractive)
                    .is_ok()
            }));
        }
        barrier.wait();
        let winners = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
    }

    #[test]
    fn expiry_and_wrong_purpose_consume_the_token() {
        let store = AuthorizationStore::default();
        let now = Instant::now();
        let (expired, _) = store
            .authorize_direct_at(
                binding(AuthorizationPurpose::CompareInteractive),
                Vec::new(),
                false,
                now,
            )
            .unwrap();
        assert!(store
            .consume_at(
                &expired.token,
                AuthorizationPurpose::CompareInteractive,
                now + AUTHORIZATION_TTL,
            )
            .is_err());

        let (wrong, _) = store
            .authorize_direct_at(
                binding(AuthorizationPurpose::CompareInteractive),
                Vec::new(),
                false,
                now,
            )
            .unwrap();
        assert!(store.consume_apply_at(&wrong.token, now).is_err());
        assert!(store
            .consume_at(&wrong.token, AuthorizationPurpose::CompareInteractive, now,)
            .is_err());
    }

    #[test]
    fn grants_cannot_cross_revision_target_scope_or_unattended_policy() {
        let store = AuthorizationStore::default();
        let spec = ChallengeSpec {
            binding: binding(AuthorizationPurpose::ApplyInteractive),
            selected: selection(),
            requires_health_ack: false,
            requires_capability_ack: true,
        };
        let (challenge, _) = store.challenge(spec).unwrap();
        store.approve(&challenge, false, true, true, false).unwrap();
        let expected = binding(AuthorizationPurpose::ApplyUnattended);
        assert!(store.grant_allows(&expected, false));
        assert!(!store.grant_allows(&expected, true));
        let mut renamed = expected.clone();
        renamed.job_name = "archive".into();
        renamed.owner.as_mut().unwrap().job_name = "archive".into();
        assert!(store.grant_allows(&renamed, false));
        let mut changed = expected.clone();
        changed.config_revision = "revision-b".into();
        assert!(!store.grant_allows(&changed, false));
        changed = expected.clone();
        changed.target_index = 0;
        assert!(!store.grant_allows(&changed, false));
        changed = expected;
        changed.scope = CapabilityScope::CompareRead;
        assert!(!store.grant_allows(&changed, false));
    }

    #[test]
    fn challenge_binds_the_exact_apply_selection() {
        let store = AuthorizationStore::default();
        let selected = selection();
        let mut exact = binding(AuthorizationPurpose::ApplyInteractive);
        exact.decision_digest = Some(decision_digest(&selected).unwrap());
        let (challenge, _) = store
            .challenge(ChallengeSpec {
                binding: exact,
                selected: selected.clone(),
                requires_health_ack: false,
                requires_capability_ack: false,
            })
            .unwrap();
        let (authorization, _) = store
            .approve(&challenge, false, false, false, false)
            .unwrap();
        assert_eq!(authorization.selected, selected);

        let mismatched = ChallengeSpec {
            binding: binding(AuthorizationPurpose::ApplyInteractive),
            selected: vec![SelectedRowDto {
                index: 4,
                flipped: false,
            }],
            requires_health_ack: false,
            requires_capability_ack: false,
        };
        assert!(store.challenge(mismatched).is_err());
    }

    #[test]
    fn purpose_scope_and_compare_owner_shape_are_enforced() {
        let store = AuthorizationStore::default();

        assert!(store
            .authorize_direct(
                binding(AuthorizationPurpose::ApplyInteractive),
                selection(),
                false,
            )
            .is_err());

        let mut wrong_scope = binding(AuthorizationPurpose::ApplyInteractive);
        wrong_scope.scope = CapabilityScope::CompareRead;
        assert!(store
            .challenge(ChallengeSpec {
                binding: wrong_scope,
                selected: selection(),
                requires_health_ack: false,
                requires_capability_ack: false,
            })
            .is_err());

        let mut wrong_owner = binding(AuthorizationPurpose::ApplyInteractive);
        wrong_owner.owner.as_mut().unwrap().job_id = "another-job".into();
        assert!(store
            .challenge(ChallengeSpec {
                binding: wrong_owner,
                selected: selection(),
                requires_health_ack: false,
                requires_capability_ack: false,
            })
            .is_err());

        let mut missing_plan = binding(AuthorizationPurpose::ApplyInteractive);
        missing_plan.plan_digest = None;
        assert!(store
            .challenge(ChallengeSpec {
                binding: missing_plan,
                selected: selection(),
                requires_health_ack: false,
                requires_capability_ack: false,
            })
            .is_err());

        assert!(store
            .challenge(ChallengeSpec {
                binding: binding(AuthorizationPurpose::ApplyInteractive),
                selected: Vec::new(),
                requires_health_ack: false,
                requires_capability_ack: false,
            })
            .is_err());

        let mut compare_with_apply_provenance = binding(AuthorizationPurpose::CompareInteractive);
        compare_with_apply_provenance.owner = Some(owner());
        assert!(store
            .authorize_direct(compare_with_apply_provenance, Vec::new(), false)
            .is_err());

        assert!(store
            .authorize_direct(
                binding(AuthorizationPurpose::CompareInteractive),
                vec![SelectedRowDto {
                    index: 0,
                    flipped: false,
                }],
                false,
            )
            .is_err());
    }

    #[test]
    fn a_later_session_grant_replaces_and_can_downgrade_the_same_key() {
        let store = AuthorizationStore::default();
        let (allow, _) = store
            .challenge(ChallengeSpec {
                binding: binding(AuthorizationPurpose::ApplyInteractive),
                selected: selection(),
                requires_health_ack: false,
                requires_capability_ack: true,
            })
            .unwrap();
        store.approve(&allow, false, true, true, true).unwrap();
        let (outstanding, _) = store
            .authorize_unattended(binding(AuthorizationPurpose::ApplyUnattended), selection())
            .unwrap();

        let (downgrade, _) = store
            .challenge(ChallengeSpec {
                binding: binding(AuthorizationPurpose::ApplyInteractive),
                selected: selection(),
                requires_health_ack: false,
                requires_capability_ack: true,
            })
            .unwrap();
        store.approve(&downgrade, false, true, true, false).unwrap();

        assert_eq!(store.0.lock().unwrap().grants.len(), 1);
        assert!(store.grant_allows(&binding(AuthorizationPurpose::ApplyInteractive), false));
        assert!(!store.grant_allows(&binding(AuthorizationPurpose::ApplyUnattended), true));
        assert!(store
            .consume(&outstanding.token, AuthorizationPurpose::ApplyUnattended,)
            .is_err());
        assert!(store
            .authorize_unattended(binding(AuthorizationPurpose::ApplyUnattended), selection(),)
            .is_err());
    }

    #[test]
    fn unattended_lookup_requires_an_unattended_apply_binding() {
        let store = AuthorizationStore::default();
        assert!(store
            .challenge(ChallengeSpec {
                binding: binding(AuthorizationPurpose::ApplyUnattended),
                selected: selection(),
                requires_health_ack: false,
                requires_capability_ack: false,
            })
            .is_err());
        assert!(store
            .authorize_unattended(binding(AuthorizationPurpose::ApplyUnattended), selection(),)
            .is_err());
        let (challenge, _) = store
            .challenge(ChallengeSpec {
                binding: binding(AuthorizationPurpose::ApplyInteractive),
                selected: selection(),
                requires_health_ack: false,
                requires_capability_ack: true,
            })
            .unwrap();
        store.approve(&challenge, false, true, true, true).unwrap();

        assert!(!store.grant_allows(&binding(AuthorizationPurpose::ApplyInteractive), true));
        assert!(store.grant_allows(&binding(AuthorizationPurpose::ApplyUnattended), true));
        assert!(store
            .authorize_direct(
                binding(AuthorizationPurpose::ApplyUnattended),
                selection(),
                false,
            )
            .is_err());
        let (authorization, _) = store
            .authorize_unattended(binding(AuthorizationPurpose::ApplyUnattended), selection())
            .unwrap();
        assert!(!authorization.acknowledged_health);
    }

    #[test]
    fn a_clean_apply_review_can_grant_unattended_authority() {
        let store = AuthorizationStore::default();
        let (challenge, _) = store
            .challenge(ChallengeSpec {
                binding: binding(AuthorizationPurpose::ApplyInteractive),
                selected: selection(),
                requires_health_ack: false,
                requires_capability_ack: false,
            })
            .unwrap();
        store.approve(&challenge, false, false, true, true).unwrap();
        assert!(store.grant_allows(&binding(AuthorizationPurpose::ApplyUnattended), true));

        let (compare, _) = store
            .challenge(ChallengeSpec {
                binding: binding(AuthorizationPurpose::CompareInteractive),
                selected: Vec::new(),
                requires_health_ack: false,
                requires_capability_ack: false,
            })
            .unwrap();
        assert!(store.approve(&compare, false, false, true, false).is_err());
    }

    #[test]
    fn decision_digest_normalizes_order_but_not_index_or_direction() {
        let first = vec![
            SelectedRowDto {
                index: 2,
                flipped: true,
            },
            SelectedRowDto {
                index: 1,
                flipped: false,
            },
        ];
        let reversed = vec![first[1].clone(), first[0].clone()];
        assert_eq!(
            decision_digest(&first).unwrap(),
            decision_digest(&reversed).unwrap()
        );
        let mut changed = first.clone();
        changed[0].flipped = false;
        assert_ne!(
            decision_digest(&first).unwrap(),
            decision_digest(&changed).unwrap()
        );
        assert!(decision_digest(&[first[0].clone(), first[0].clone()]).is_err());
    }

    #[test]
    fn health_digest_normalizes_message_order_but_binds_acknowledgement_effect() {
        let unacknowledged = syncdash::pipeline::guard::Verdict {
            blockers: vec![
                "warning must be acknowledged".into(),
                "root is missing".into(),
            ],
            warnings: vec!["space is low".into()],
        };
        let acknowledged = syncdash::pipeline::guard::Verdict {
            blockers: vec!["root is missing".into()],
            warnings: vec!["space is low".into()],
        };
        let mut reordered_unacknowledged = unacknowledged.clone();
        reordered_unacknowledged.blockers.reverse();
        assert_eq!(
            health_digest(&unacknowledged, &acknowledged),
            health_digest(&reordered_unacknowledged, &acknowledged)
        );

        let changed_acknowledged = syncdash::pipeline::guard::Verdict {
            blockers: Vec::new(),
            warnings: acknowledged.warnings.clone(),
        };
        assert_ne!(
            health_digest(&unacknowledged, &acknowledged),
            health_digest(&unacknowledged, &changed_acknowledged)
        );
    }

    #[test]
    fn stores_are_bounded_and_job_revocation_is_exact() {
        let store = AuthorizationStore::default();
        for index in 0..(AUTHORIZATION_CAPACITY + 5) {
            let mut next = binding(AuthorizationPurpose::CompareInteractive);
            next.job_id = format!("job-{index}");
            store.authorize_direct(next, Vec::new(), false).unwrap();
        }
        assert_eq!(
            store.0.lock().unwrap().authorizations.len(),
            AUTHORIZATION_CAPACITY
        );
        store.revoke_job("job-9");
        assert!(store
            .0
            .lock()
            .unwrap()
            .authorizations
            .iter()
            .all(|record| record.binding.job_id != "job-9"));
    }
}
