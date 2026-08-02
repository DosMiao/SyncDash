//! Durable Compare evidence and process-local execution freshness.

mod persistence;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use persistence::{CompareResultPersistence, LoadedCompareResults};

const HOT_RESULT_CACHE_CAPACITY: usize = 4;

use crate::dto::{
    CompareExecutionExpiryReasonDto, CompareIdentity, CompareOwner, CompareScopeDto,
    CompareScopeExecutionStatusDto, CompareVerificationAttemptDto, CompareWorkspaceLookupDto,
    CompareWorkspaceSnapshotDto, PlanDto,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CompareResultRepositoryError {
    Storage(String),
    DuplicateIdentity(CompareIdentity),
    IdentityMismatch {
        result_id: String,
    },
    MissingJobDisplayName {
        job_id: String,
    },
    DanglingLatestVersion(CompareIdentity),
    AwaitingSuccessfulCompare(CompareScope),
    ResultIsNotExecutionFresh {
        requested_run_id: u64,
        fresh_run_id: u64,
    },
    FreshResultWasNotRetained(CompareIdentity),
    VerificationEpochExhausted(CompareScope),
    VerificationScopeMismatch,
    VerificationWasSuperseded {
        submitted_epoch: u64,
        active_epoch: u64,
    },
    VerificationHasNotLaunched(CompareScope),
    VerificationRunMismatch {
        launched_run_id: u64,
        published_run_id: u64,
    },
    VerificationAlreadyPublished(CompareIdentity),
    VerificationIsNotActive(CompareIdentity),
}

impl std::fmt::Display for CompareResultRepositoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(message) => formatter.write_str(message),
            Self::DuplicateIdentity(_) => {
                formatter.write_str("This exact Compare result identity is already retained")
            }
            Self::IdentityMismatch { result_id } => write!(
                formatter,
                "Compare result ID '{result_id}' belongs to a different immutable identity"
            ),
            Self::MissingJobDisplayName { job_id } => write!(
                formatter,
                "The retained Compare repository lost the presentation name for job '{job_id}'"
            ),
            Self::DanglingLatestVersion(identity) => write!(
                formatter,
                "The latest Compare pointer for run {} has no retained version",
                identity.compare_run_id
            ),
            Self::AwaitingSuccessfulCompare(scope) => write!(
                formatter,
                "A newer Compare or AutoScan verification started for job '{}' target {} and has not published a successful result — wait for it to succeed or run Compare again",
                scope.job_id,
                scope.target_index + 1
            ),
            Self::ResultIsNotExecutionFresh {
                requested_run_id,
                fresh_run_id,
            } => write!(
                formatter,
                "Compare run {} is retained for viewing but run {} is the execution-eligible result for this job target",
                requested_run_id, fresh_run_id
            ),
            Self::FreshResultWasNotRetained(identity) => write!(
                formatter,
                "Execution-eligible Compare run {} is no longer retained — run Compare again",
                identity.compare_run_id
            ),
            Self::VerificationEpochExhausted(scope) => write!(
                formatter,
                "The Compare verification sequence for job '{}' target {} is exhausted — restart SyncDash",
                scope.job_id,
                scope.target_index + 1
            ),
            Self::VerificationScopeMismatch => write!(
                formatter,
                "The successful Compare does not belong to its verification ticket's exact job revision and target"
            ),
            Self::VerificationWasSuperseded {
                submitted_epoch,
                active_epoch,
            } => write!(
                formatter,
                "Compare verification epoch {submitted_epoch} was superseded by epoch {active_epoch}"
            ),
            Self::VerificationHasNotLaunched(scope) => write!(
                formatter,
                "The Compare verification for job '{}' target {} has not launched a run",
                scope.job_id,
                scope.target_index + 1
            ),
            Self::VerificationRunMismatch {
                launched_run_id,
                published_run_id,
            } => write!(
                formatter,
                "Compare run {published_run_id} cannot publish a verification launched by run {launched_run_id}"
            ),
            Self::VerificationAlreadyPublished(identity) => write!(
                formatter,
                "The verification ticket already published execution-eligible Compare run {}",
                identity.compare_run_id
            ),
            Self::VerificationIsNotActive(identity) => write!(
                formatter,
                "The verification for Compare run {} already reached a terminal status",
                identity.compare_run_id
            ),
        }
    }
}

impl std::error::Error for CompareResultRepositoryError {}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CompareScope {
    pub(super) job_id: String,
    pub(super) target_index: usize,
    pub(super) config_revision: String,
}

impl CompareScope {
    pub(crate) fn new(job_id: &str, target_index: usize, config_revision: &str) -> Self {
        Self {
            job_id: job_id.to_string(),
            target_index,
            config_revision: config_revision.to_string(),
        }
    }

    pub(super) fn from_identity(identity: &CompareIdentity) -> Self {
        Self::new(
            &identity.job_id,
            identity.target_index,
            &identity.config_revision,
        )
    }

    pub(crate) fn contains(&self, identity: &CompareIdentity) -> bool {
        self == &Self::from_identity(identity)
    }

    fn dto(&self) -> CompareScopeDto {
        CompareScopeDto {
            job_id: self.job_id.clone(),
            target_index: self.target_index,
            config_revision: self.config_revision.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CompareExecutionState {
    AwaitingCompare {
        verification_epoch: u64,
    },
    Comparing {
        verification_epoch: u64,
        launched_run_id: u64,
    },
    Fresh {
        verification_epoch: u64,
        identity: CompareIdentity,
    },
    Failed {
        verification_epoch: u64,
        launched_run_id: Option<u64>,
        message: String,
    },
    Cancelled {
        verification_epoch: u64,
        launched_run_id: Option<u64>,
    },
    Expired {
        verification_epoch: u64,
        launched_run_id: Option<u64>,
        reason: CompareExecutionExpiryReasonDto,
    },
}

impl CompareExecutionState {
    fn verification_epoch(&self) -> u64 {
        match self {
            Self::AwaitingCompare { verification_epoch }
            | Self::Comparing {
                verification_epoch, ..
            }
            | Self::Fresh {
                verification_epoch, ..
            }
            | Self::Failed {
                verification_epoch, ..
            }
            | Self::Cancelled {
                verification_epoch, ..
            }
            | Self::Expired {
                verification_epoch, ..
            } => *verification_epoch,
        }
    }

    fn launched_run_id(&self) -> Option<u64> {
        match self {
            Self::AwaitingCompare { .. } => None,
            Self::Comparing {
                launched_run_id, ..
            } => Some(*launched_run_id),
            Self::Fresh { identity, .. } => Some(identity.compare_run_id),
            Self::Failed {
                launched_run_id, ..
            }
            | Self::Cancelled {
                launched_run_id, ..
            }
            | Self::Expired {
                launched_run_id, ..
            } => *launched_run_id,
        }
    }

    fn verification_attempt(&self) -> CompareVerificationAttemptDto {
        CompareVerificationAttemptDto {
            verification_epoch: self.verification_epoch(),
            compare_run_id: self.launched_run_id(),
        }
    }

    fn expire(&mut self, reason: CompareExecutionExpiryReasonDto) {
        let verification_epoch = self.verification_epoch();
        let launched_run_id = self.launched_run_id();
        *self = Self::Expired {
            verification_epoch,
            launched_run_id,
            reason,
        };
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompareVerificationTicket {
    scope: CompareScope,
    epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompareVerificationTerminalOutcome {
    Failed { message: String },
    Cancelled,
}

struct RetainedPlan {
    header: syncdash::model::plan::PlanHeader,
    operations: Vec<syncdash::model::plan::Op>,
    metadata: Vec<Option<syncdash::pipeline::compare::evidence::RowMeta>>,
    identical_count: u64,
    identical_bytes: u64,
}

pub(crate) struct SuccessfulCompareResult {
    owner: CompareOwner,
    plan_digest: String,
    plan: RetainedPlan,
    source: syncdash::model::table::TableArtifact,
    target: syncdash::model::table::TableArtifact,
    compare_options: syncdash::pipeline::compare::CompareOptions,
}

pub(crate) struct SuccessfulComparePublication {
    pub(crate) workspace: CompareWorkspaceSnapshotDto,
}

impl SuccessfulCompareResult {
    pub(crate) fn from_plan(
        plan_digest: String,
        plan: PlanDto,
        source: syncdash::model::table::TableArtifact,
        target: syncdash::model::table::TableArtifact,
        compare_options: syncdash::pipeline::compare::CompareOptions,
    ) -> Self {
        let PlanDto {
            header,
            ops,
            metas,
            identical_count,
            identical_bytes,
            owner,
        } = plan;
        Self {
            owner,
            plan_digest,
            plan: RetainedPlan {
                header,
                operations: ops,
                metadata: metas,
                identical_count,
                identical_bytes,
            },
            source,
            target,
            compare_options,
        }
    }

    pub(crate) fn owner(&self) -> &CompareOwner {
        &self.owner
    }

    fn into_version(self) -> (CompareResultVersion, String) {
        (
            CompareResultVersion {
                identity: self.owner.identity,
                plan_digest: self.plan_digest,
                plan: self.plan,
                source: self.source,
                target: self.target,
                compare_options: self.compare_options,
            },
            self.owner.job_name,
        )
    }
}

struct CompareResultVersion {
    identity: CompareIdentity,
    plan_digest: String,
    plan: RetainedPlan,
    source: syncdash::model::table::TableArtifact,
    target: syncdash::model::table::TableArtifact,
    compare_options: syncdash::pipeline::compare::CompareOptions,
}

pub(crate) struct RetainedCompareResult {
    version: Arc<CompareResultVersion>,
    owner: CompareOwner,
}

impl RetainedCompareResult {
    pub(crate) fn identity(&self) -> &CompareIdentity {
        &self.version.identity
    }

    pub(crate) fn owner(&self) -> &CompareOwner {
        &self.owner
    }

    pub(crate) fn plan_digest(&self) -> &str {
        &self.version.plan_digest
    }

    pub(crate) fn plan(&self) -> PlanDto {
        PlanDto {
            header: self.version.plan.header.clone(),
            ops: self.version.plan.operations.clone(),
            metas: self.version.plan.metadata.clone(),
            identical_count: self.version.plan.identical_count,
            identical_bytes: self.version.plan.identical_bytes,
            owner: self.owner.clone(),
        }
    }

    pub(crate) fn plan_header(&self) -> &syncdash::model::plan::PlanHeader {
        &self.version.plan.header
    }

    pub(crate) fn plan_operations(&self) -> &[syncdash::model::plan::Op] {
        &self.version.plan.operations
    }

    pub(crate) fn plan_metadata(
        &self,
    ) -> &[Option<syncdash::pipeline::compare::evidence::RowMeta>] {
        &self.version.plan.metadata
    }

    pub(crate) fn source(&self) -> &syncdash::model::table::TableArtifact {
        &self.version.source
    }

    pub(crate) fn target(&self) -> &syncdash::model::table::TableArtifact {
        &self.version.target
    }

    pub(crate) fn compare_options(&self) -> &syncdash::pipeline::compare::CompareOptions {
        &self.version.compare_options
    }
}

struct CompareResultStore {
    versions_by_id: HashMap<String, Arc<CompareResultVersion>>,
    cache_order: std::collections::VecDeque<String>,
    retained_identities_by_id: HashMap<String, CompareIdentity>,
    cache_capacity: usize,
    latest_by_scope: HashMap<CompareScope, CompareIdentity>,
    execution_by_scope: HashMap<CompareScope, CompareExecutionState>,
    job_names: HashMap<String, String>,
    persistence_generation: u64,
}

impl CompareResultStore {
    #[cfg(test)]
    fn empty() -> Self {
        Self {
            versions_by_id: HashMap::new(),
            cache_order: std::collections::VecDeque::new(),
            retained_identities_by_id: HashMap::new(),
            cache_capacity: usize::MAX,
            latest_by_scope: HashMap::new(),
            execution_by_scope: HashMap::new(),
            job_names: HashMap::new(),
            persistence_generation: 0,
        }
    }

    fn from_loaded(loaded: LoadedCompareResults) -> Self {
        let execution_by_scope = loaded
            .latest_by_scope
            .iter()
            .map(|(scope, identity)| {
                (
                    scope.clone(),
                    CompareExecutionState::Expired {
                        verification_epoch: 0,
                        launched_run_id: Some(identity.compare_run_id),
                        reason: CompareExecutionExpiryReasonDto::ApplicationRestarted,
                    },
                )
            })
            .collect();
        Self {
            versions_by_id: HashMap::new(),
            cache_order: std::collections::VecDeque::new(),
            retained_identities_by_id: loaded.identities_by_id,
            cache_capacity: HOT_RESULT_CACHE_CAPACITY,
            latest_by_scope: loaded.latest_by_scope,
            execution_by_scope,
            job_names: loaded.job_names,
            persistence_generation: loaded.generation,
        }
    }

    fn execution_status(&self, scope: &CompareScope) -> CompareScopeExecutionStatusDto {
        let Some(state) = self.execution_by_scope.get(scope) else {
            return CompareScopeExecutionStatusDto::Unavailable { scope: scope.dto() };
        };
        let attempt = state.verification_attempt();
        let scope = scope.dto();
        match state {
            CompareExecutionState::AwaitingCompare { .. } => {
                CompareScopeExecutionStatusDto::AwaitingCompare { scope, attempt }
            }
            CompareExecutionState::Comparing { .. } => {
                CompareScopeExecutionStatusDto::Comparing { scope, attempt }
            }
            CompareExecutionState::Fresh { identity, .. } => {
                CompareScopeExecutionStatusDto::Fresh {
                    scope,
                    attempt,
                    owner: CompareOwner {
                        identity: identity.clone(),
                        job_name: self
                            .job_names
                            .get(&identity.job_id)
                            .cloned()
                            .unwrap_or_else(|| {
                                panic!(
                                "execution-fresh Compare run {} has no retained presentation name",
                                identity.compare_run_id
                            )
                            }),
                    },
                }
            }
            CompareExecutionState::Failed { message, .. } => {
                CompareScopeExecutionStatusDto::Failed {
                    scope,
                    attempt,
                    message: message.clone(),
                }
            }
            CompareExecutionState::Cancelled { .. } => {
                CompareScopeExecutionStatusDto::Cancelled { scope, attempt }
            }
            CompareExecutionState::Expired { reason, .. } => {
                CompareScopeExecutionStatusDto::Expired {
                    scope,
                    attempt,
                    reason: *reason,
                }
            }
        }
    }

    fn expire_scope(&mut self, scope: &CompareScope, reason: CompareExecutionExpiryReasonDto) {
        if let Some(state) = self.execution_by_scope.get_mut(scope) {
            state.expire(reason);
        }
    }

    fn validate_successful_publication(
        &self,
        verification: &CompareVerificationTicket,
        identity: &CompareIdentity,
    ) -> Result<(), CompareResultRepositoryError> {
        let scope = CompareScope::from_identity(identity);
        if verification.scope != scope {
            return Err(CompareResultRepositoryError::VerificationScopeMismatch);
        }
        let state = self
            .execution_by_scope
            .get(&verification.scope)
            .expect("a repository-issued verification ticket must retain execution state");
        if state.verification_epoch() != verification.epoch {
            return Err(CompareResultRepositoryError::VerificationWasSuperseded {
                submitted_epoch: verification.epoch,
                active_epoch: state.verification_epoch(),
            });
        }
        match state {
            CompareExecutionState::AwaitingCompare { .. } => {
                return Err(CompareResultRepositoryError::VerificationHasNotLaunched(
                    scope,
                ));
            }
            CompareExecutionState::Comparing {
                launched_run_id, ..
            } => {
                if *launched_run_id != identity.compare_run_id {
                    return Err(CompareResultRepositoryError::VerificationRunMismatch {
                        launched_run_id: *launched_run_id,
                        published_run_id: identity.compare_run_id,
                    });
                }
            }
            CompareExecutionState::Fresh {
                identity: published,
                ..
            } => {
                return Err(CompareResultRepositoryError::VerificationAlreadyPublished(
                    published.clone(),
                ));
            }
            CompareExecutionState::Failed { .. }
            | CompareExecutionState::Cancelled { .. }
            | CompareExecutionState::Expired { .. } => {
                return Err(CompareResultRepositoryError::VerificationIsNotActive(
                    identity.clone(),
                ));
            }
        }
        if self
            .retained_identities_by_id
            .contains_key(&identity.result_id)
        {
            return Err(CompareResultRepositoryError::DuplicateIdentity(
                identity.clone(),
            ));
        }
        Ok(())
    }

    fn commit_successful_publication(
        &mut self,
        verification: &CompareVerificationTicket,
        version: Arc<CompareResultVersion>,
        job_name: String,
        persistence_generation: u64,
    ) -> Result<SuccessfulComparePublication, CompareResultRepositoryError> {
        let identity = version.identity.clone();
        let scope = CompareScope::from_identity(&identity);
        self.job_names.insert(identity.job_id.clone(), job_name);
        self.retained_identities_by_id
            .insert(identity.result_id.clone(), identity.clone());
        self.cache_version(version);
        self.latest_by_scope.insert(scope.clone(), identity.clone());
        self.persistence_generation = persistence_generation;
        *self
            .execution_by_scope
            .get_mut(&scope)
            .expect("the current verification ticket must have execution state") =
            CompareExecutionState::Fresh {
                verification_epoch: verification.epoch,
                identity: identity.clone(),
            };
        let workspace = match self.exact_workspace_lookup(&identity)? {
            CompareWorkspaceLookupDto::Found { workspace } => *workspace,
            CompareWorkspaceLookupDto::Missing { .. } => {
                return Err(CompareResultRepositoryError::FreshResultWasNotRetained(
                    identity,
                ));
            }
        };
        Ok(SuccessfulComparePublication { workspace })
    }

    fn cached_exact(
        &mut self,
        identity: &CompareIdentity,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        if !self.exact_identity_is_retained(identity)? {
            return Ok(None);
        }
        let Some(version) = self.versions_by_id.get(&identity.result_id).cloned() else {
            return Ok(None);
        };
        self.touch_cache(&identity.result_id);
        self.retained_result(version).map(Some)
    }

    fn exact_identity_is_retained(
        &self,
        identity: &CompareIdentity,
    ) -> Result<bool, CompareResultRepositoryError> {
        match self.retained_identities_by_id.get(&identity.result_id) {
            None => Ok(false),
            Some(retained) if retained == identity => Ok(true),
            Some(_) => Err(CompareResultRepositoryError::IdentityMismatch {
                result_id: identity.result_id.clone(),
            }),
        }
    }

    fn cache_version(&mut self, version: Arc<CompareResultVersion>) {
        let result_id = version.identity.result_id.clone();
        self.versions_by_id.insert(result_id.clone(), version);
        self.touch_cache(&result_id);
        while self.cache_order.len() > self.cache_capacity {
            let evicted = self
                .cache_order
                .pop_back()
                .expect("an over-capacity result cache contains an entry");
            self.versions_by_id.remove(&evicted);
        }
    }

    fn touch_cache(&mut self, result_id: &str) {
        if let Some(index) = self
            .cache_order
            .iter()
            .position(|cached| cached == result_id)
        {
            self.cache_order.remove(index);
        }
        self.cache_order.push_front(result_id.to_string());
    }

    fn retained_result(
        &self,
        version: Arc<CompareResultVersion>,
    ) -> Result<RetainedCompareResult, CompareResultRepositoryError> {
        let job_name = self
            .job_names
            .get(&version.identity.job_id)
            .cloned()
            .ok_or_else(|| CompareResultRepositoryError::MissingJobDisplayName {
                job_id: version.identity.job_id.clone(),
            })?;
        Ok(RetainedCompareResult {
            owner: CompareOwner {
                identity: version.identity.clone(),
                job_name,
            },
            version,
        })
    }

    fn begin_verification(
        &mut self,
        scope: CompareScope,
        launched_run_id: Option<u64>,
    ) -> Result<CompareVerificationTicket, CompareResultRepositoryError> {
        let epoch = match self.execution_by_scope.get_mut(&scope) {
            Some(state) => {
                let Some(epoch) = state.verification_epoch().checked_add(1) else {
                    state.expire(CompareExecutionExpiryReasonDto::VerificationExhausted);
                    return Err(CompareResultRepositoryError::VerificationEpochExhausted(
                        scope,
                    ));
                };
                epoch
            }
            None => 1,
        };
        let state = match launched_run_id {
            Some(launched_run_id) => CompareExecutionState::Comparing {
                verification_epoch: epoch,
                launched_run_id,
            },
            None => CompareExecutionState::AwaitingCompare {
                verification_epoch: epoch,
            },
        };
        self.execution_by_scope.insert(scope.clone(), state);
        Ok(CompareVerificationTicket { scope, epoch })
    }

    fn mark_verification_comparing(
        &mut self,
        verification: &CompareVerificationTicket,
        launched_run_id: u64,
    ) -> bool {
        let Some(state) = self.execution_by_scope.get_mut(&verification.scope) else {
            return false;
        };
        match state {
            CompareExecutionState::AwaitingCompare { verification_epoch }
                if *verification_epoch == verification.epoch =>
            {
                *state = CompareExecutionState::Comparing {
                    verification_epoch: verification.epoch,
                    launched_run_id,
                };
                true
            }
            _ => false,
        }
    }

    fn complete_verification_terminal(
        &mut self,
        verification: &CompareVerificationTicket,
        outcome: CompareVerificationTerminalOutcome,
    ) -> bool {
        let Some(state) = self.execution_by_scope.get_mut(&verification.scope) else {
            return false;
        };
        let launched_run_id = match state {
            CompareExecutionState::AwaitingCompare { verification_epoch }
                if *verification_epoch == verification.epoch =>
            {
                None
            }
            CompareExecutionState::Comparing {
                verification_epoch,
                launched_run_id,
            } if *verification_epoch == verification.epoch => Some(*launched_run_id),
            _ => return false,
        };
        *state = match outcome {
            CompareVerificationTerminalOutcome::Failed { message } => {
                CompareExecutionState::Failed {
                    verification_epoch: verification.epoch,
                    launched_run_id,
                    message,
                }
            }
            CompareVerificationTerminalOutcome::Cancelled => CompareExecutionState::Cancelled {
                verification_epoch: verification.epoch,
                launched_run_id,
            },
        };
        true
    }

    fn ensure_execution_fresh(
        &self,
        identity: &CompareIdentity,
    ) -> Result<(), CompareResultRepositoryError> {
        let scope = CompareScope::from_identity(identity);
        match self.execution_by_scope.get(&scope) {
            Some(CompareExecutionState::Fresh {
                identity: fresh, ..
            }) if fresh == identity => {
                if self.retained_identities_by_id.get(&identity.result_id) == Some(identity) {
                    Ok(())
                } else {
                    Err(CompareResultRepositoryError::FreshResultWasNotRetained(
                        identity.clone(),
                    ))
                }
            }
            Some(CompareExecutionState::Fresh {
                identity: fresh, ..
            }) => Err(CompareResultRepositoryError::ResultIsNotExecutionFresh {
                requested_run_id: identity.compare_run_id,
                fresh_run_id: fresh.compare_run_id,
            }),
            Some(
                CompareExecutionState::AwaitingCompare { .. }
                | CompareExecutionState::Comparing { .. }
                | CompareExecutionState::Failed { .. }
                | CompareExecutionState::Cancelled { .. }
                | CompareExecutionState::Expired { .. },
            )
            | None => Err(CompareResultRepositoryError::AwaitingSuccessfulCompare(
                scope,
            )),
        }
    }

    fn exact_workspace_lookup(
        &mut self,
        identity: &CompareIdentity,
    ) -> Result<CompareWorkspaceLookupDto, CompareResultRepositoryError> {
        let scope = CompareScope::from_identity(identity);
        let Some(retained) = self.cached_exact(identity)? else {
            return Ok(CompareWorkspaceLookupDto::Missing {
                execution_status: self.execution_status(&scope),
            });
        };
        Ok(CompareWorkspaceLookupDto::Found {
            workspace: Box::new(CompareWorkspaceSnapshotDto {
                plan: retained.plan(),
                execution_status: self.execution_status(&scope),
            }),
        })
    }

    fn reconcile_exact_workspace(
        &mut self,
        identity: &CompareIdentity,
        job_state: CompareWorkspaceJobState,
    ) -> Result<CompareWorkspaceLookupDto, CompareResultRepositoryError> {
        let scope = CompareScope::from_identity(identity);
        match job_state {
            CompareWorkspaceJobState::Current { job_name } => {
                self.rebind_job_name(&identity.job_id, &job_name);
            }
            CompareWorkspaceJobState::ConfigurationChanged => {
                self.expire_scope(&scope, CompareExecutionExpiryReasonDto::JobChanged);
            }
            CompareWorkspaceJobState::Deleted => {
                self.expire_scope(&scope, CompareExecutionExpiryReasonDto::JobDeleted);
            }
        }
        self.exact_workspace_lookup(identity)
    }

    fn latest_identity(&self, scope: &CompareScope) -> Option<CompareIdentity> {
        self.latest_by_scope.get(scope).cloned()
    }

    fn expire_revision(
        &mut self,
        job_id: &str,
        config_revision: &str,
        reason: CompareExecutionExpiryReasonDto,
    ) {
        let scopes = self
            .execution_by_scope
            .keys()
            .filter(|scope| scope.job_id == job_id && scope.config_revision == config_revision)
            .cloned()
            .collect::<Vec<_>>();
        for scope in scopes {
            self.expire_scope(&scope, reason);
        }
    }

    fn expire_job(&mut self, job_id: &str) {
        let scopes = self
            .execution_by_scope
            .keys()
            .filter(|scope| scope.job_id == job_id)
            .cloned()
            .collect::<Vec<_>>();
        for scope in scopes {
            self.expire_scope(&scope, CompareExecutionExpiryReasonDto::JobDeleted);
        }
    }

    fn rebind_job_name(&mut self, job_id: &str, job_name: &str) {
        if self
            .retained_identities_by_id
            .values()
            .any(|identity| identity.job_id == job_id)
        {
            self.job_names
                .insert(job_id.to_string(), job_name.to_string());
        }
    }

    fn forget(&mut self, identity: &CompareIdentity, persistence_generation: u64) {
        self.retained_identities_by_id.remove(&identity.result_id);
        self.versions_by_id.remove(&identity.result_id);
        self.cache_order
            .retain(|cached| cached != &identity.result_id);
        let scope = CompareScope::from_identity(identity);
        if self.latest_by_scope.get(&scope) == Some(identity) {
            self.latest_by_scope.remove(&scope);
        }
        if matches!(
            self.execution_by_scope.get(&scope),
            Some(CompareExecutionState::Fresh { identity: fresh, .. }) if fresh == identity
        ) {
            self.execution_by_scope.remove(&scope);
        }
        self.job_names.retain(|job_id, _| {
            self.retained_identities_by_id
                .values()
                .any(|identity| identity.job_id == *job_id)
        });
        self.persistence_generation = persistence_generation;
    }
}

pub(crate) struct CompareResultRepository {
    store: Mutex<CompareResultStore>,
    persistence: Option<CompareResultPersistence>,
}

pub(crate) enum CompareWorkspaceJobState {
    Current { job_name: String },
    ConfigurationChanged,
    Deleted,
}

pub(crate) enum CompareResultForgetOutcome {
    Forgotten { cleanup_warning: Option<String> },
    AlreadyForgotten,
}

#[cfg(test)]
impl Default for CompareResultRepository {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl CompareResultRepository {
    pub(crate) fn open_default() -> Result<Self, CompareResultRepositoryError> {
        let (persistence, loaded) = CompareResultPersistence::open_default()
            .map_err(|error| storage_error("Cannot open durable Compare results", error))?;
        Ok(Self {
            store: Mutex::new(CompareResultStore::from_loaded(loaded)),
            persistence: Some(persistence),
        })
    }

    #[cfg(test)]
    fn in_memory() -> Self {
        Self {
            store: Mutex::new(CompareResultStore::empty()),
            persistence: None,
        }
    }

    #[cfg(test)]
    fn open_at(path: std::path::PathBuf) -> Result<Self, CompareResultRepositoryError> {
        let (persistence, loaded) = CompareResultPersistence::open_at(path)
            .map_err(|error| storage_error("Cannot open durable Compare results", error))?;
        Ok(Self {
            store: Mutex::new(CompareResultStore::from_loaded(loaded)),
            persistence: Some(persistence),
        })
    }

    pub(crate) fn publish_successful_version(
        &self,
        verification: &CompareVerificationTicket,
        version: SuccessfulCompareResult,
    ) -> Result<SuccessfulComparePublication, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        store.validate_successful_publication(verification, &version.owner.identity)?;
        let (version, job_name) = version.into_version();
        let persisted = match &self.persistence {
            Some(persistence) => persistence
                .publish(store.persistence_generation, version, &job_name)
                .map_err(|error| {
                    storage_error("Cannot retain the successful Compare result", error)
                })?,
            None => persistence::PersistedPublication {
                generation: store.persistence_generation.checked_add(1).ok_or_else(|| {
                    CompareResultRepositoryError::Storage(
                        "The in-memory Compare-result generation is exhausted".to_string(),
                    )
                })?,
                version: Arc::new(version),
            },
        };
        store.commit_successful_publication(
            verification,
            persisted.version,
            job_name,
            persisted.generation,
        )
    }

    pub(crate) fn get_exact(
        &self,
        identity: &CompareIdentity,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        self.load_exact_locked(&mut store, identity)
    }

    pub(crate) fn get_fresh_exact(
        &self,
        identity: &CompareIdentity,
    ) -> Result<RetainedCompareResult, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        store.ensure_execution_fresh(identity)?;
        self.load_exact_locked(&mut store, identity)?
            .ok_or_else(|| {
                CompareResultRepositoryError::FreshResultWasNotRetained(identity.clone())
            })
    }

    pub(crate) fn begin_verification(
        &self,
        scope: CompareScope,
        launched_run_id: Option<u64>,
    ) -> Result<CompareVerificationTicket, CompareResultRepositoryError> {
        self.store
            .lock()
            .unwrap()
            .begin_verification(scope, launched_run_id)
    }

    pub(crate) fn mark_verification_comparing(
        &self,
        verification: &CompareVerificationTicket,
        launched_run_id: u64,
    ) -> bool {
        self.store
            .lock()
            .unwrap()
            .mark_verification_comparing(verification, launched_run_id)
    }

    pub(crate) fn complete_verification_terminal(
        &self,
        verification: &CompareVerificationTicket,
        outcome: CompareVerificationTerminalOutcome,
    ) -> bool {
        self.store
            .lock()
            .unwrap()
            .complete_verification_terminal(verification, outcome)
    }

    /// Keep the freshness lock through a short reservation edge, so a newer verification cannot
    /// invalidate the result between the check and reservation. The callback must not call back
    /// into this repository.
    pub(crate) fn with_fresh_execution_eligibility<T>(
        &self,
        identity: &CompareIdentity,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let store = self.store.lock().unwrap();
        store
            .ensure_execution_fresh(identity)
            .map_err(|error| error.to_string())?;
        operation()
    }

    #[cfg(test)]
    pub(crate) fn latest_for(
        &self,
        job_id: &str,
        target_index: usize,
        config_revision: &str,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        let scope = CompareScope::new(job_id, target_index, config_revision);
        let Some(identity) = store.latest_identity(&scope) else {
            return Ok(None);
        };
        self.load_exact_locked(&mut store, &identity)?.map_or_else(
            || {
                Err(CompareResultRepositoryError::DanglingLatestVersion(
                    identity,
                ))
            },
            |retained| Ok(Some(retained)),
        )
    }

    pub(crate) fn reconcile_exact_workspace(
        &self,
        identity: &CompareIdentity,
        job_state: CompareWorkspaceJobState,
    ) -> Result<CompareWorkspaceLookupDto, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        if let CompareWorkspaceJobState::Current { job_name } = &job_state {
            self.rebind_job_name_locked(&mut store, &identity.job_id, job_name)?;
        }
        self.load_exact_locked(&mut store, identity)?;
        store.reconcile_exact_workspace(identity, job_state)
    }

    pub(crate) fn restore_workspace(
        &self,
        job_id: &str,
        target_index: usize,
        config_revision: &str,
    ) -> Result<CompareWorkspaceLookupDto, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        let scope = CompareScope::new(job_id, target_index, config_revision);
        let Some(identity) = store.latest_identity(&scope) else {
            return Ok(CompareWorkspaceLookupDto::Missing {
                execution_status: store.execution_status(&scope),
            });
        };
        let retained = self.load_exact_locked(&mut store, &identity)?.ok_or(
            CompareResultRepositoryError::DanglingLatestVersion(identity),
        )?;
        Ok(CompareWorkspaceLookupDto::Found {
            workspace: Box::new(CompareWorkspaceSnapshotDto {
                plan: retained.plan(),
                execution_status: store.execution_status(&scope),
            }),
        })
    }

    pub(crate) fn execution_status(&self, scope: &CompareScope) -> CompareScopeExecutionStatusDto {
        let store = self.store.lock().unwrap();
        store.execution_status(scope)
    }

    #[cfg(test)]
    pub(crate) fn execution_status_for_identity(
        &self,
        identity: &CompareIdentity,
    ) -> CompareScopeExecutionStatusDto {
        self.execution_status(&CompareScope::from_identity(identity))
    }

    pub(crate) fn expire_revision(
        &self,
        job_id: &str,
        config_revision: &str,
        reason: CompareExecutionExpiryReasonDto,
    ) -> Vec<CompareScopeExecutionStatusDto> {
        let mut store = self.store.lock().unwrap();
        store.expire_revision(job_id, config_revision, reason);
        store
            .execution_by_scope
            .keys()
            .filter(|scope| scope.job_id == job_id && scope.config_revision == config_revision)
            .map(|scope| store.execution_status(scope))
            .collect()
    }

    pub(crate) fn expire_job(&self, job_id: &str) -> Vec<CompareScopeExecutionStatusDto> {
        let mut store = self.store.lock().unwrap();
        store.expire_job(job_id);
        store
            .execution_by_scope
            .keys()
            .filter(|scope| scope.job_id == job_id)
            .map(|scope| store.execution_status(scope))
            .collect()
    }

    pub(crate) fn rebind_job_name(
        &self,
        job_id: &str,
        job_name: &str,
    ) -> Result<(), CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        self.rebind_job_name_locked(&mut store, job_id, job_name)
    }

    pub(crate) fn forget(
        &self,
        identity: &CompareIdentity,
    ) -> Result<CompareResultForgetOutcome, CompareResultRepositoryError> {
        let mut store = self.store.lock().unwrap();
        if !store.exact_identity_is_retained(identity)? {
            return Ok(CompareResultForgetOutcome::AlreadyForgotten);
        }
        let outcome = match &self.persistence {
            Some(persistence) => persistence
                .forget(store.persistence_generation, identity)
                .map_err(|error| storage_error("Cannot forget the Compare result", error))?,
            None => persistence::PersistedForget {
                generation: store.persistence_generation.checked_add(1).ok_or_else(|| {
                    CompareResultRepositoryError::Storage(
                        "The in-memory Compare-result generation is exhausted".to_string(),
                    )
                })?,
                cleanup_error: None,
            },
        };
        store.forget(identity, outcome.generation);
        Ok(CompareResultForgetOutcome::Forgotten {
            cleanup_warning: outcome.cleanup_error,
        })
    }

    fn load_exact_locked(
        &self,
        store: &mut CompareResultStore,
        identity: &CompareIdentity,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        if !store.exact_identity_is_retained(identity)? {
            return Ok(None);
        }
        if let Some(retained) = store.cached_exact(identity)? {
            return Ok(Some(retained));
        }
        let Some(persistence) = &self.persistence else {
            return Err(CompareResultRepositoryError::FreshResultWasNotRetained(
                identity.clone(),
            ));
        };
        let version = persistence
            .load_exact(store.persistence_generation, identity)
            .map_err(|error| storage_error("Cannot load the exact Compare result", error))?;
        store.cache_version(version.clone());
        store.retained_result(version).map(Some)
    }

    fn rebind_job_name_locked(
        &self,
        store: &mut CompareResultStore,
        job_id: &str,
        job_name: &str,
    ) -> Result<(), CompareResultRepositoryError> {
        if let Some(persistence) = &self.persistence {
            if let Some(generation) = persistence
                .rebind_job_name(store.persistence_generation, job_id, job_name)
                .map_err(|error| {
                    storage_error("Cannot update retained Compare presentation", error)
                })?
            {
                store.persistence_generation = generation;
            }
        }
        store.rebind_job_name(job_id, job_name);
        Ok(())
    }
}

fn storage_error(context: &str, error: std::io::Error) -> CompareResultRepositoryError {
    CompareResultRepositoryError::Storage(format!("{context}: {error}"))
}

pub(crate) fn validate_retained_compare(
    retained: Option<&RetainedCompareResult>,
    owner: &CompareOwner,
    job_id: &str,
    job_name: &str,
    target_index: usize,
    config_revision: &str,
    plan_digest: Option<&str>,
) -> Result<(), String> {
    if owner.identity.job_id != job_id {
        return Err(format!(
            "This Compare result belongs to a different job identity than '{job_name}' — run Compare again"
        ));
    }
    if owner.identity.target_index != target_index {
        return Err(format!(
            "This Compare result belongs to target {}, not target {} — run Compare again",
            owner.identity.target_index + 1,
            target_index + 1
        ));
    }
    if owner.identity.config_revision != config_revision {
        return Err(format!(
            "Job '{job_name}' changed since this Compare — run Compare again"
        ));
    }
    let Some(retained) = retained else {
        return Err("This exact Compare result is no longer retained — run Compare again".into());
    };
    if retained.identity() != &owner.identity {
        return Err("The retained Compare result identity changed — run Compare again".into());
    }
    if let Some(plan_digest) = plan_digest {
        if retained.plan_digest() != plan_digest {
            return Err(
                "This Compare result no longer matches the plan produced by Compare — run Compare again"
                    .into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use syncdash::model::plan::PlanHeader;
    use syncdash::model::table::{
        TableArtifact, TableEvidence, TableHeader, TableKind, TABLE_SCHEMA,
    };

    fn identity(
        job_id: &str,
        target_index: usize,
        revision: &str,
        compare_run_id: u64,
    ) -> CompareIdentity {
        let result_digest = blake3::hash(
            format!("{job_id}\0{target_index}\0{revision}\0{compare_run_id}").as_bytes(),
        )
        .to_hex()
        .to_string();
        CompareIdentity {
            result_id: result_digest[..32].to_string(),
            compare_run_id,
            job_id: job_id.into(),
            target_index,
            config_revision: revision.into(),
        }
    }

    fn owner(
        job_id: &str,
        job_name: &str,
        target_index: usize,
        revision: &str,
        compare_run_id: u64,
    ) -> CompareOwner {
        CompareOwner {
            identity: identity(job_id, target_index, revision, compare_run_id),
            job_name: job_name.into(),
        }
    }

    fn version(
        job_id: &str,
        job_name: &str,
        target_index: usize,
        revision: &str,
        compare_run_id: u64,
    ) -> SuccessfulCompareResult {
        let owner = owner(job_id, job_name, target_index, revision, compare_run_id);
        let plan_header = PlanHeader {
            schema: syncdash::model::plan::PLAN_SCHEMA,
            kind: "plan".into(),
            mode: "mirror".into(),
            generated_at_ms: 0,
            source_root: "/source".into(),
            source_host: "host".into(),
            target_root: "/target".into(),
            target_host: "host".into(),
            op_count: 0,
            conflict_count: 0,
            source_entries: 0,
            target_entries: 0,
            source_excluded: 0,
            target_excluded: 0,
            source_walk_errors: 0,
            target_walk_errors: 0,
            source_walk_err_samples: Vec::new(),
            target_walk_err_samples: Vec::new(),
            source_icloud_stubs: 0,
            target_icloud_stubs: 0,
            source_icloud_stub_samples: Vec::new(),
            target_icloud_stub_samples: Vec::new(),
        };
        let plan_digest = syncdash::model::plan::Plan::digest_parts(&plan_header, &[]);
        let snapshot = |root: &str| TableArtifact {
            header: TableHeader {
                schema: TABLE_SCHEMA,
                kind: TableKind::Snapshot,
                root: root.into(),
                host: "host".into(),
                os: "test".into(),
                scanned_at_ms: 0,
                duration_ms: 0,
                entry_count: 0,
                evidence: TableEvidence::None,
                excluded_dirs: 0,
                excluded_files: 0,
                walk_errors: 0,
                walk_err_samples: Vec::new(),
                icloud_stubs: 0,
                icloud_stub_samples: Vec::new(),
                skipped_symlinks: 0,
                dataless_files: 0,
                vfs: None,
            },
            entries: Vec::new(),
        };
        SuccessfulCompareResult::from_plan(
            plan_digest,
            PlanDto {
                header: plan_header,
                ops: Vec::new(),
                metas: Vec::new(),
                identical_count: 0,
                identical_bytes: 0,
                owner,
            },
            snapshot("/source"),
            snapshot("/target"),
            syncdash::pipeline::compare::CompareOptions::default(),
        )
    }

    fn publish(repository: &CompareResultRepository, version: SuccessfulCompareResult) {
        let scope = CompareScope::from_identity(&version.owner.identity);
        let compare_run_id = version.owner.identity.compare_run_id;
        let verification = repository
            .begin_verification(scope, Some(compare_run_id))
            .unwrap();
        repository
            .publish_successful_version(&verification, version)
            .unwrap();
    }

    #[test]
    fn exact_versions_survive_newer_publications_for_the_same_scope() {
        let repository = CompareResultRepository::in_memory();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        publish(&repository, version("job-a", "A", 0, "revision-a", 2));

        let older = repository
            .get_exact(&identity("job-a", 0, "revision-a", 1))
            .unwrap()
            .unwrap();
        assert_eq!(older.identity().compare_run_id, 1);
        let latest = repository
            .latest_for("job-a", 0, "revision-a")
            .unwrap()
            .unwrap();
        assert_eq!(latest.identity().compare_run_id, 2);
    }

    #[test]
    fn failed_or_cancelled_newer_compare_preserves_display_but_blocks_execution() {
        let repository = CompareResultRepository::in_memory();
        let retained_identity = identity("job-a", 0, "revision-a", 1);
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        assert!(repository.get_fresh_exact(&retained_identity).is_ok());

        let failed = repository
            .begin_verification(CompareScope::new("job-a", 0, "revision-a"), Some(2))
            .unwrap();
        assert!(repository.complete_verification_terminal(
            &failed,
            CompareVerificationTerminalOutcome::Failed {
                message: "network unavailable".into(),
            },
        ));

        assert!(repository.get_exact(&retained_identity).unwrap().is_some());
        assert_eq!(
            repository
                .latest_for("job-a", 0, "revision-a")
                .unwrap()
                .unwrap()
                .identity(),
            &retained_identity
        );
        let error = match repository.get_fresh_exact(&retained_identity) {
            Err(error) => error,
            Ok(_) => panic!("a failed newer Compare must leave the retained result non-executable"),
        };
        assert!(matches!(
            error,
            CompareResultRepositoryError::AwaitingSuccessfulCompare(_)
        ));
        let mut reserved = false;
        assert!(repository
            .with_fresh_execution_eligibility(&retained_identity, || {
                reserved = true;
                Ok(())
            })
            .is_err());
        assert!(!reserved);
        assert!(matches!(
            repository.execution_status_for_identity(&retained_identity),
            CompareScopeExecutionStatusDto::Failed { attempt, message, .. }
                if attempt.verification_epoch == 2
                    && attempt.compare_run_id == Some(2)
                    && message == "network unavailable"
        ));
    }

    #[test]
    fn successful_republication_restores_only_the_new_exact_result() {
        let repository = CompareResultRepository::in_memory();
        let older_identity = identity("job-a", 0, "revision-a", 1);
        let newer_identity = identity("job-a", 0, "revision-a", 2);
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        let verification = repository
            .begin_verification(CompareScope::new("job-a", 0, "revision-a"), Some(2))
            .unwrap();
        repository
            .publish_successful_version(&verification, version("job-a", "A", 0, "revision-a", 2))
            .unwrap();

        assert!(repository.get_exact(&older_identity).unwrap().is_some());
        assert!(matches!(
            repository.get_fresh_exact(&older_identity),
            Err(CompareResultRepositoryError::ResultIsNotExecutionFresh { .. })
        ));
        assert_eq!(
            repository
                .get_fresh_exact(&newer_identity)
                .unwrap()
                .identity(),
            &newer_identity
        );
        assert_eq!(
            repository
                .with_fresh_execution_eligibility(&newer_identity, || Ok("reserved"))
                .unwrap(),
            "reserved"
        );
    }

    #[test]
    fn awaiting_verification_cannot_publish_before_a_compare_run_launches() {
        let repository = CompareResultRepository::in_memory();
        let scope = CompareScope::new("job-a", 0, "revision-a");
        let verification = repository.begin_verification(scope.clone(), None).unwrap();
        let result_identity = identity("job-a", 0, "revision-a", 8);

        assert!(matches!(
            repository.publish_successful_version(
                &verification,
                version("job-a", "A", 0, "revision-a", 8),
            ),
            Err(CompareResultRepositoryError::VerificationHasNotLaunched(
                rejected_scope,
            )) if rejected_scope == scope
        ));
        assert!(repository.get_exact(&result_identity).unwrap().is_none());
        assert!(matches!(
            repository.execution_status(&scope),
            CompareScopeExecutionStatusDto::AwaitingCompare { attempt, .. }
                if attempt.verification_epoch == 1 && attempt.compare_run_id.is_none()
        ));
    }

    #[test]
    fn launched_verification_rejects_a_result_from_another_run() {
        let repository = CompareResultRepository::in_memory();
        let scope = CompareScope::new("job-a", 0, "revision-a");
        let verification = repository
            .begin_verification(scope.clone(), Some(8))
            .unwrap();
        let wrong_identity = identity("job-a", 0, "revision-a", 9);

        assert!(matches!(
            repository.publish_successful_version(
                &verification,
                version("job-a", "A", 0, "revision-a", 9),
            ),
            Err(CompareResultRepositoryError::VerificationRunMismatch {
                launched_run_id: 8,
                published_run_id: 9,
            })
        ));
        assert!(repository.get_exact(&wrong_identity).unwrap().is_none());
        assert!(matches!(
            repository.execution_status(&scope),
            CompareScopeExecutionStatusDto::Comparing { attempt, .. }
                if attempt.verification_epoch == 1 && attempt.compare_run_id == Some(8)
        ));
    }

    #[test]
    fn superseded_success_is_rejected_without_retaining_evidence() {
        let repository = CompareResultRepository::in_memory();
        let scope = CompareScope::new("job-a", 0, "revision-a");
        let first = repository
            .begin_verification(scope.clone(), Some(1))
            .unwrap();
        let second = repository.begin_verification(scope, Some(2)).unwrap();
        let first_identity = identity("job-a", 0, "revision-a", 1);
        let second_identity = identity("job-a", 0, "revision-a", 2);

        assert!(matches!(
            repository
                .publish_successful_version(&first, version("job-a", "A", 0, "revision-a", 1),),
            Err(CompareResultRepositoryError::VerificationWasSuperseded {
                submitted_epoch: 1,
                active_epoch: 2,
            })
        ));
        assert!(repository.get_exact(&first_identity).unwrap().is_none());
        assert!(matches!(
            repository.execution_status_for_identity(&first_identity),
            CompareScopeExecutionStatusDto::Comparing { attempt, .. }
                if attempt.verification_epoch == 2 && attempt.compare_run_id == Some(2)
        ));
        assert!(matches!(
            repository.get_fresh_exact(&first_identity),
            Err(CompareResultRepositoryError::AwaitingSuccessfulCompare(_))
        ));
        repository
            .publish_successful_version(&second, version("job-a", "A", 0, "revision-a", 2))
            .unwrap();
        assert_eq!(
            repository
                .get_fresh_exact(&second_identity)
                .unwrap()
                .identity(),
            &second_identity
        );
    }

    #[test]
    fn late_older_verification_cannot_publish_or_regress_current_pointers() {
        let repository = CompareResultRepository::in_memory();
        let scope = CompareScope::new("job-a", 0, "revision-a");
        let first = repository
            .begin_verification(scope.clone(), Some(1))
            .unwrap();
        let second = repository.begin_verification(scope, Some(2)).unwrap();
        let first_identity = identity("job-a", 0, "revision-a", 1);
        let second_identity = identity("job-a", 0, "revision-a", 2);

        repository
            .publish_successful_version(&second, version("job-a", "A", 0, "revision-a", 2))
            .unwrap();
        assert!(matches!(
            repository
                .publish_successful_version(&first, version("job-a", "A", 0, "revision-a", 1),),
            Err(CompareResultRepositoryError::VerificationWasSuperseded {
                submitted_epoch: 1,
                active_epoch: 2,
            })
        ));

        assert!(repository.get_exact(&first_identity).unwrap().is_none());
        assert_eq!(
            repository
                .latest_for("job-a", 0, "revision-a")
                .unwrap()
                .unwrap()
                .identity(),
            &second_identity
        );
        assert_eq!(
            repository
                .get_fresh_exact(&second_identity)
                .unwrap()
                .identity(),
            &second_identity
        );
    }

    #[test]
    fn verification_epoch_exhaustion_stays_fail_closed() {
        let repository = CompareResultRepository::in_memory();
        let scope = CompareScope::new("job-a", 0, "revision-a");
        repository.store.lock().unwrap().execution_by_scope.insert(
            scope.clone(),
            CompareExecutionState::Fresh {
                verification_epoch: u64::MAX,
                identity: identity("job-a", 0, "revision-a", 1),
            },
        );

        assert!(matches!(
            repository.begin_verification(scope, Some(2)),
            Err(CompareResultRepositoryError::VerificationEpochExhausted(_))
        ));
        assert!(matches!(
            repository.get_fresh_exact(&identity("job-a", 0, "revision-a", 1)),
            Err(CompareResultRepositoryError::AwaitingSuccessfulCompare(_))
        ));
    }

    #[test]
    fn final_reservation_and_new_verification_have_one_lock_order() {
        let repository = Arc::new(CompareResultRepository::in_memory());
        let result_identity = identity("job-a", 0, "revision-a", 1);
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        let (reservation_entered, entered) = std::sync::mpsc::channel();
        let (release_reservation, release) = std::sync::mpsc::channel();
        let reservation_repository = repository.clone();
        let reserved_identity = result_identity.clone();
        let reservation = std::thread::spawn(move || {
            reservation_repository.with_fresh_execution_eligibility(&reserved_identity, || {
                reservation_entered.send(()).unwrap();
                release.recv().unwrap();
                Ok(())
            })
        });
        entered.recv().unwrap();

        let verification_repository = repository.clone();
        let (verification_began, began) = std::sync::mpsc::channel();
        let verification = std::thread::spawn(move || {
            let ticket = verification_repository
                .begin_verification(CompareScope::new("job-a", 0, "revision-a"), Some(2))
                .unwrap();
            verification_began.send(ticket).unwrap();
        });
        assert!(began
            .recv_timeout(std::time::Duration::from_millis(25))
            .is_err());

        release_reservation.send(()).unwrap();
        reservation.join().unwrap().unwrap();
        began.recv().unwrap();
        verification.join().unwrap();
        assert!(matches!(
            repository.get_fresh_exact(&result_identity),
            Err(CompareResultRepositoryError::AwaitingSuccessfulCompare(_))
        ));
    }

    #[test]
    fn bounded_hot_cache_never_changes_retention_or_latest_pointers() {
        let repository = CompareResultRepository::in_memory();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        publish(&repository, version("job-b", "B", 0, "revision-b", 2));
        publish(&repository, version("job-c", "C", 0, "revision-c", 3));
        publish(&repository, version("job-d", "D", 0, "revision-d", 4));
        publish(&repository, version("job-e", "E", 0, "revision-e", 5));

        assert!(repository
            .latest_for("job-b", 0, "revision-b")
            .unwrap()
            .is_some());
        assert!(repository
            .latest_for("job-a", 0, "revision-a")
            .unwrap()
            .is_some());
    }

    #[test]
    fn explicit_forget_removes_only_the_exact_result_and_its_latest_pointer() {
        let repository = CompareResultRepository::in_memory();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        publish(&repository, version("job-b", "B", 0, "revision-b", 2));
        let forgotten = identity("job-b", 0, "revision-b", 2);
        assert!(matches!(
            repository.forget(&forgotten).unwrap(),
            CompareResultForgetOutcome::Forgotten {
                cleanup_warning: None
            }
        ));
        assert!(matches!(
            repository.forget(&forgotten).unwrap(),
            CompareResultForgetOutcome::AlreadyForgotten
        ));
        assert!(repository.get_exact(&forgotten).unwrap().is_none());
        assert!(repository
            .get_exact(&identity("job-a", 0, "revision-a", 1))
            .unwrap()
            .is_some());
        assert!(matches!(
            repository
                .restore_workspace("job-b", 0, "revision-b")
                .unwrap(),
            CompareWorkspaceLookupDto::Missing { .. }
        ));
    }

    #[test]
    fn result_id_with_different_identity_fields_fails_closed() {
        let repository = CompareResultRepository::in_memory();
        let retained = identity("job-a", 0, "revision-a", 1);
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        let mut mismatched = retained.clone();
        mismatched.config_revision = "revision-b".to_string();

        assert!(matches!(
            repository.get_exact(&mismatched),
            Err(CompareResultRepositoryError::IdentityMismatch { result_id })
                if result_id == retained.result_id
        ));
        assert!(matches!(
            repository.forget(&mismatched),
            Err(CompareResultRepositoryError::IdentityMismatch { result_id })
                if result_id == retained.result_id
        ));
        assert!(repository.get_exact(&retained).unwrap().is_some());
    }

    #[test]
    fn mutation_expiry_is_scoped_by_stable_job_identity_and_revision_without_deleting_evidence() {
        let repository = CompareResultRepository::in_memory();
        publish(&repository, version("job-a", "A", 0, "revision-old", 1));
        publish(&repository, version("job-a", "A", 0, "revision-current", 2));
        publish(&repository, version("job-b", "A", 0, "revision-old", 3));

        repository.expire_revision(
            "job-a",
            "revision-old",
            CompareExecutionExpiryReasonDto::JobChanged,
        );
        assert!(repository
            .get_exact(&identity("job-a", 0, "revision-old", 1))
            .unwrap()
            .is_some());
        assert!(repository
            .get_exact(&identity("job-a", 0, "revision-current", 2))
            .unwrap()
            .is_some());
        assert!(repository
            .get_exact(&identity("job-b", 0, "revision-old", 3))
            .unwrap()
            .is_some());

        assert!(matches!(
            repository.execution_status_for_identity(&identity("job-a", 0, "revision-old", 1)),
            CompareScopeExecutionStatusDto::Expired {
                reason: CompareExecutionExpiryReasonDto::JobChanged,
                ..
            }
        ));

        repository.expire_revision(
            "job-a",
            "revision-current",
            CompareExecutionExpiryReasonDto::WriteStarted,
        );
        assert!(matches!(
            repository.execution_status_for_identity(&identity("job-a", 0, "revision-current", 2)),
            CompareScopeExecutionStatusDto::Expired {
                reason: CompareExecutionExpiryReasonDto::WriteStarted,
                ..
            }
        ));

        repository.expire_job("job-a");
        assert!(repository
            .get_exact(&identity("job-a", 0, "revision-current", 2))
            .unwrap()
            .is_some());
        assert!(matches!(
            repository.execution_status_for_identity(&identity("job-a", 0, "revision-current", 2)),
            CompareScopeExecutionStatusDto::Expired {
                reason: CompareExecutionExpiryReasonDto::JobDeleted,
                ..
            }
        ));
    }

    #[test]
    fn workspace_restore_reads_plan_and_execution_status_under_one_scope_epoch() {
        let repository = CompareResultRepository::in_memory();
        let scope = CompareScope::new("job-a", 0, "revision-a");
        let verification = repository
            .begin_verification(scope.clone(), Some(41))
            .unwrap();
        repository
            .publish_successful_version(&verification, version("job-a", "A", 0, "revision-a", 41))
            .unwrap();

        let workspace = repository
            .restore_workspace("job-a", 0, "revision-a")
            .unwrap();
        let CompareWorkspaceLookupDto::Found { workspace } = workspace else {
            panic!("the published Compare result must restore");
        };
        assert_eq!(workspace.plan.owner.identity.compare_run_id, 41);
        assert!(matches!(
            workspace.execution_status,
            CompareScopeExecutionStatusDto::Fresh { attempt, owner, .. }
                if attempt.verification_epoch == 1
                    && attempt.compare_run_id == Some(41)
                    && owner.identity == workspace.plan.owner.identity
        ));
    }

    #[test]
    fn stale_terminal_completion_cannot_overwrite_a_newer_attempt_status() {
        let repository = CompareResultRepository::in_memory();
        let scope = CompareScope::new("job-a", 0, "revision-a");
        let older = repository
            .begin_verification(scope.clone(), Some(10))
            .unwrap();
        let current = repository
            .begin_verification(scope.clone(), Some(11))
            .unwrap();

        assert!(!repository.complete_verification_terminal(
            &older,
            CompareVerificationTerminalOutcome::Failed {
                message: "late failure".into(),
            },
        ));
        assert!(repository.complete_verification_terminal(
            &current,
            CompareVerificationTerminalOutcome::Cancelled,
        ));
        assert!(matches!(
            repository.execution_status(&scope),
            CompareScopeExecutionStatusDto::Cancelled { attempt, .. }
                if attempt.verification_epoch == 2 && attempt.compare_run_id == Some(11)
        ));
    }

    #[test]
    fn typed_prelaunch_terminal_states_do_not_invent_a_compare_run_identity() {
        let repository = CompareResultRepository::in_memory();
        let scope = CompareScope::new("job-a", 0, "revision-a");
        let cancelled = repository.begin_verification(scope.clone(), None).unwrap();

        assert!(repository.complete_verification_terminal(
            &cancelled,
            CompareVerificationTerminalOutcome::Cancelled,
        ));
        assert!(matches!(
            repository.execution_status(&scope),
            CompareScopeExecutionStatusDto::Cancelled { attempt, .. }
                if attempt.verification_epoch == 1 && attempt.compare_run_id.is_none()
        ));

        let failed = repository.begin_verification(scope.clone(), None).unwrap();
        assert!(repository.complete_verification_terminal(
            &failed,
            CompareVerificationTerminalOutcome::Failed {
                message: "cancelled".into(),
            },
        ));
        assert!(matches!(
            repository.execution_status(&scope),
            CompareScopeExecutionStatusDto::Failed { attempt, message, .. }
                if attempt.verification_epoch == 2
                    && attempt.compare_run_id.is_none()
                    && message == "cancelled"
        ));

        repository.expire_revision(
            "job-a",
            "revision-a",
            CompareExecutionExpiryReasonDto::JobChanged,
        );
        assert!(matches!(
            repository.execution_status(&scope),
            CompareScopeExecutionStatusDto::Expired { attempt, reason, .. }
                if attempt.verification_epoch == 2
                    && attempt.compare_run_id.is_none()
                    && reason == CompareExecutionExpiryReasonDto::JobChanged
        ));
    }

    #[test]
    fn terminal_failure_cannot_be_reopened_by_a_late_success() {
        let repository = CompareResultRepository::in_memory();
        let scope = CompareScope::new("job-a", 0, "revision-a");
        let verification = repository.begin_verification(scope, Some(12)).unwrap();

        assert!(repository.complete_verification_terminal(
            &verification,
            CompareVerificationTerminalOutcome::Cancelled,
        ));
        assert!(!repository.complete_verification_terminal(
            &verification,
            CompareVerificationTerminalOutcome::Failed {
                message: "late failure".into(),
            },
        ));
        assert!(matches!(
            repository.publish_successful_version(
                &verification,
                version("job-a", "A", 0, "revision-a", 12),
            ),
            Err(CompareResultRepositoryError::VerificationIsNotActive(_))
        ));
    }

    #[test]
    fn reconciliation_after_configuration_change_keeps_the_plan_view_only() {
        let repository = CompareResultRepository::in_memory();
        let result = identity("job-a", 0, "revision-a", 7);
        publish(&repository, version("job-a", "A", 0, "revision-a", 7));

        let workspace = repository
            .reconcile_exact_workspace(&result, CompareWorkspaceJobState::ConfigurationChanged)
            .unwrap();
        let CompareWorkspaceLookupDto::Found { workspace } = workspace else {
            panic!("the retained Compare plan must remain viewable after expiry");
        };
        assert_eq!(workspace.plan.owner.identity, result);
        assert!(matches!(
            workspace.execution_status,
            CompareScopeExecutionStatusDto::Expired {
                reason: CompareExecutionExpiryReasonDto::JobChanged,
                ..
            }
        ));
        assert!(repository.get_fresh_exact(&result).is_err());
    }

    #[test]
    fn missing_workspace_returns_its_terminal_execution_status_atomically() {
        let repository = CompareResultRepository::in_memory();
        let result = identity("job-a", 0, "revision-a", 7);
        repository
            .begin_verification(
                CompareScope::from_identity(&result),
                Some(result.compare_run_id),
            )
            .unwrap();

        let lookup = repository
            .reconcile_exact_workspace(&result, CompareWorkspaceJobState::Deleted)
            .unwrap();

        assert!(matches!(
            lookup,
            CompareWorkspaceLookupDto::Missing {
                execution_status: CompareScopeExecutionStatusDto::Expired {
                    scope,
                    attempt,
                    reason: CompareExecutionExpiryReasonDto::JobDeleted,
                },
            } if scope == CompareScope::from_identity(&result).dto()
                && attempt.verification_epoch == 1
                && attempt.compare_run_id == Some(7)
        ));
    }

    #[test]
    fn rename_rebinds_only_presentation_for_every_retained_version() {
        let repository = CompareResultRepository::in_memory();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        publish(&repository, version("job-a", "A", 0, "revision-a", 2));

        repository.rebind_job_name("job-a", "Archive").unwrap();
        let older = repository
            .get_exact(&identity("job-a", 0, "revision-a", 1))
            .unwrap()
            .unwrap();
        assert_eq!(older.owner().job_name, "Archive");
        assert_eq!(older.plan().owner.job_name, "Archive");
        assert_eq!(older.identity().compare_run_id, 1);
    }

    #[test]
    fn validation_requires_the_exact_retained_identity_and_plan_digest() {
        let repository = CompareResultRepository::in_memory();
        publish(&repository, version("job-a", "A", 1, "revision-a", 7));
        let owner = owner("job-a", "A", 1, "revision-a", 7);
        let retained = repository.get_exact(&owner.identity).unwrap();
        let plan_digest = retained.as_ref().unwrap().plan_digest().to_string();
        assert!(validate_retained_compare(
            retained.as_ref(),
            &owner,
            "job-a",
            "A",
            1,
            "revision-a",
            Some(&plan_digest),
        )
        .is_ok());

        let wrong_digest = validate_retained_compare(
            retained.as_ref(),
            &owner,
            "job-a",
            "A",
            1,
            "revision-a",
            Some("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        )
        .unwrap_err();
        assert!(wrong_digest.contains("no longer matches"), "{wrong_digest}");

        let missing = validate_retained_compare(None, &owner, "job-a", "A", 1, "revision-a", None)
            .unwrap_err();
        assert!(missing.contains("exact Compare result"), "{missing}");
    }

    #[test]
    fn missing_presentation_state_fails_closed_instead_of_reusing_a_stale_plan_label() {
        let repository = CompareResultRepository::in_memory();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        repository.store.lock().unwrap().job_names.remove("job-a");

        let error = match repository.get_exact(&identity("job-a", 0, "revision-a", 1)) {
            Err(error) => error,
            Ok(_) => panic!("missing presentation state must fail closed"),
        };
        assert_eq!(
            error,
            CompareResultRepositoryError::MissingJobDisplayName {
                job_id: "job-a".into()
            }
        );
    }

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let token = crate::authority_token::random_hex::<8>("test directory").unwrap();
            let path = std::env::temp_dir().join(format!(
                "syncdash-compare-results-{label}-{}-{token}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn durable_result_restores_after_restart_but_execution_is_expired() {
        let directory = TestDirectory::new("restart");
        let result = identity("job-a", 0, "revision-a", 1);
        {
            let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
            publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        }

        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        assert!(repository.store.lock().unwrap().versions_by_id.is_empty());
        let restored = repository
            .restore_workspace("job-a", 0, "revision-a")
            .unwrap();
        assert!(matches!(
            restored,
            CompareWorkspaceLookupDto::Found { workspace }
                if workspace.plan.owner.identity == result
                    && matches!(
                        workspace.execution_status,
                        CompareScopeExecutionStatusDto::Expired {
                            reason: CompareExecutionExpiryReasonDto::ApplicationRestarted,
                            ..
                        }
                    )
        ));
        assert!(repository.get_fresh_exact(&result).is_err());
    }

    #[test]
    fn stable_result_id_prevents_run_number_collision_after_restart() {
        let directory = TestDirectory::new("run-id-collision");
        let older = identity("job-a", 0, "revision-a", 1);
        {
            let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
            publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        }

        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        let mut newer_version = version("job-a", "A", 0, "revision-a", 1);
        newer_version.owner.identity.result_id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into();
        let newer = newer_version.owner.identity.clone();
        publish(&repository, newer_version);

        assert_eq!(older.compare_run_id, newer.compare_run_id);
        assert_ne!(older.result_id, newer.result_id);
        assert!(repository.get_exact(&older).unwrap().is_some());
        assert_eq!(
            repository
                .latest_for("job-a", 0, "revision-a")
                .unwrap()
                .unwrap()
                .identity(),
            &newer
        );
    }

    #[test]
    fn hot_cache_eviction_never_changes_durable_retention() {
        let directory = TestDirectory::new("hot-cache");
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        for run_id in 1..=6 {
            publish(
                &repository,
                version(
                    &format!("job-{run_id}"),
                    &format!("Job {run_id}"),
                    0,
                    &format!("revision-{run_id}"),
                    run_id,
                ),
            );
        }
        {
            let store = repository.store.lock().unwrap();
            assert_eq!(store.retained_identities_by_id.len(), 6);
            assert_eq!(store.versions_by_id.len(), HOT_RESULT_CACHE_CAPACITY);
        }
        assert!(repository
            .get_exact(&identity("job-1", 0, "revision-1", 1))
            .unwrap()
            .is_some());
        assert_eq!(
            repository
                .store
                .lock()
                .unwrap()
                .retained_identities_by_id
                .len(),
            6
        );
    }

    #[test]
    fn durable_forget_survives_restart_without_resurrecting_an_older_latest() {
        let directory = TestDirectory::new("forget");
        let older = identity("job-a", 0, "revision-a", 1);
        let latest = identity("job-a", 0, "revision-a", 2);
        {
            let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
            publish(&repository, version("job-a", "A", 0, "revision-a", 1));
            publish(&repository, version("job-a", "A", 0, "revision-a", 2));
            assert!(matches!(
                repository.forget(&latest).unwrap(),
                CompareResultForgetOutcome::Forgotten {
                    cleanup_warning: None
                }
            ));
        }

        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        assert!(repository.get_exact(&older).unwrap().is_some());
        assert!(repository.get_exact(&latest).unwrap().is_none());
        assert!(matches!(
            repository
                .restore_workspace("job-a", 0, "revision-a")
                .unwrap(),
            CompareWorkspaceLookupDto::Missing { .. }
        ));
    }

    #[test]
    fn corrupt_artifact_blocks_repository_startup() {
        use std::io::Write as _;

        let directory = TestDirectory::new("corrupt");
        let result = identity("job-a", 0, "revision-a", 1);
        {
            let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
            publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        }
        let artifact = directory
            .0
            .join("results")
            .join(format!("{}.jsonl", result.result_id));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&artifact)
            .unwrap()
            .write_all(b"{}\n")
            .unwrap();

        assert!(matches!(
            CompareResultRepository::open_at(directory.0.clone()),
            Err(CompareResultRepositoryError::Storage(_))
        ));
    }
}
