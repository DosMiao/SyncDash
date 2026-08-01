//! Process-local retention for successful Compare results.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::dto::{CompareIdentity, CompareOwner, PlanDto};

const DEFAULT_RETAINED_VERSION_CAPACITY: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CompareResultRepositoryError {
    DuplicateIdentity(CompareIdentity),
    MissingJobPresentation { job_id: String },
    DanglingLatestVersion(CompareIdentity),
}

impl std::fmt::Display for CompareResultRepositoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateIdentity(identity) => write!(
                formatter,
                "Compare run {} is already retained for this exact result identity",
                identity.compare_run_id
            ),
            Self::MissingJobPresentation { job_id } => write!(
                formatter,
                "The retained Compare repository lost the presentation name for job '{job_id}'"
            ),
            Self::DanglingLatestVersion(identity) => write!(
                formatter,
                "The latest Compare pointer for run {} has no retained version",
                identity.compare_run_id
            ),
        }
    }
}

impl std::error::Error for CompareResultRepositoryError {}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CompareScope {
    job_id: String,
    target_index: usize,
    config_revision: String,
}

impl CompareScope {
    fn new(job_id: &str, target_index: usize, config_revision: &str) -> Self {
        Self {
            job_id: job_id.to_string(),
            target_index,
            config_revision: config_revision.to_string(),
        }
    }

    fn from_identity(identity: &CompareIdentity) -> Self {
        Self::new(
            &identity.job_id,
            identity.target_index,
            &identity.config_revision,
        )
    }
}

struct RetainedPlan {
    header: syncdash::model::plan::PlanHeader,
    operations: Vec<syncdash::model::plan::Op>,
    metadata: Vec<Option<syncdash::pipeline::compare::evidence::RowMeta>>,
    identical_count: u64,
    identical_bytes: u64,
}

pub(crate) struct SuccessfulCompareResult {
    identity: CompareIdentity,
    job_name: String,
    plan_digest: String,
    plan: RetainedPlan,
    source: syncdash::model::table::Snapshot,
    target: syncdash::model::table::Snapshot,
    compare_options: syncdash::pipeline::compare::CompareOptions,
}

impl SuccessfulCompareResult {
    pub(crate) fn from_plan(
        plan_digest: String,
        plan: PlanDto,
        source: syncdash::model::table::Snapshot,
        target: syncdash::model::table::Snapshot,
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
            identity: owner.identity,
            job_name: owner.job_name,
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
}

struct CompareResultVersion {
    identity: CompareIdentity,
    plan_digest: String,
    plan: RetainedPlan,
    source: syncdash::model::table::Snapshot,
    target: syncdash::model::table::Snapshot,
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

    pub(crate) fn source(&self) -> &syncdash::model::table::Snapshot {
        &self.version.source
    }

    pub(crate) fn target(&self) -> &syncdash::model::table::Snapshot {
        &self.version.target
    }

    pub(crate) fn compare_options(&self) -> &syncdash::pipeline::compare::CompareOptions {
        &self.version.compare_options
    }
}

struct CompareResultStore {
    versions_by_recency: VecDeque<Arc<CompareResultVersion>>,
    latest_by_scope: HashMap<CompareScope, CompareIdentity>,
    job_names: HashMap<String, String>,
    capacity: usize,
}

impl CompareResultStore {
    fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            versions_by_recency: VecDeque::new(),
            latest_by_scope: HashMap::new(),
            job_names: HashMap::new(),
            capacity,
        }
    }

    fn insert_version(
        &mut self,
        version: SuccessfulCompareResult,
    ) -> Result<(), CompareResultRepositoryError> {
        if self
            .versions_by_recency
            .iter()
            .any(|entry| entry.identity == version.identity)
        {
            return Err(CompareResultRepositoryError::DuplicateIdentity(
                version.identity,
            ));
        }

        let identity = version.identity.clone();
        let scope = CompareScope::from_identity(&identity);
        self.job_names
            .insert(identity.job_id.clone(), version.job_name);
        self.versions_by_recency
            .push_front(Arc::new(CompareResultVersion {
                identity: version.identity,
                plan_digest: version.plan_digest,
                plan: version.plan,
                source: version.source,
                target: version.target,
                compare_options: version.compare_options,
            }));
        self.latest_by_scope.insert(scope, identity);
        self.evict_excess_versions();
        Ok(())
    }

    fn get_exact(
        &mut self,
        identity: &CompareIdentity,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        let Some(index) = self
            .versions_by_recency
            .iter()
            .position(|entry| entry.identity == *identity)
        else {
            return Ok(None);
        };
        self.touch(index).map(Some)
    }

    fn latest_for(
        &mut self,
        scope: &CompareScope,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        let Some(identity) = self.latest_by_scope.get(scope).cloned() else {
            return Ok(None);
        };
        self.get_exact(&identity)?.map_or_else(
            || {
                Err(CompareResultRepositoryError::DanglingLatestVersion(
                    identity,
                ))
            },
            |retained| Ok(Some(retained)),
        )
    }

    fn touch(
        &mut self,
        index: usize,
    ) -> Result<RetainedCompareResult, CompareResultRepositoryError> {
        let version = self
            .versions_by_recency
            .remove(index)
            .expect("a located retained Compare version must exist");
        self.versions_by_recency.push_front(version.clone());
        let job_name = self
            .job_names
            .get(&version.identity.job_id)
            .cloned()
            .ok_or_else(|| CompareResultRepositoryError::MissingJobPresentation {
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

    fn invalidate_revision(&mut self, job_id: &str, config_revision: &str) {
        self.versions_by_recency.retain(|entry| {
            entry.identity.job_id != job_id || entry.identity.config_revision != config_revision
        });
        self.latest_by_scope
            .retain(|scope, _| scope.job_id != job_id || scope.config_revision != config_revision);
        self.prune_unused_job_names();
    }

    fn invalidate_job(&mut self, job_id: &str) {
        self.versions_by_recency
            .retain(|entry| entry.identity.job_id != job_id);
        self.latest_by_scope
            .retain(|scope, _| scope.job_id != job_id);
        self.job_names.remove(job_id);
    }

    fn rebind_job_name(&mut self, job_id: &str, job_name: &str) {
        if self
            .versions_by_recency
            .iter()
            .any(|entry| entry.identity.job_id == job_id)
        {
            self.job_names
                .insert(job_id.to_string(), job_name.to_string());
        }
    }

    fn evict_excess_versions(&mut self) {
        while self.versions_by_recency.len() > self.capacity {
            // Superseded versions spend the version budget first. This keeps one latest result per
            // scope available while capacity permits, even when one hot AutoScan scope churns.
            let eviction_index = self
                .versions_by_recency
                .iter()
                .rposition(|version| {
                    let scope = CompareScope::from_identity(&version.identity);
                    self.latest_by_scope.get(&scope) != Some(&version.identity)
                })
                .unwrap_or(self.versions_by_recency.len() - 1);
            let evicted = self
                .versions_by_recency
                .remove(eviction_index)
                .expect("an over-capacity Compare repository must contain a version");
            let scope = CompareScope::from_identity(&evicted.identity);
            // "Latest" is a publication fact, not the newest survivor of LRU eviction. If that
            // exact version is gone, restore must return none instead of silently adopting an older run.
            if self.latest_by_scope.get(&scope) == Some(&evicted.identity) {
                self.latest_by_scope.remove(&scope);
            }
        }
        self.prune_unused_job_names();
    }

    fn prune_unused_job_names(&mut self) {
        self.job_names.retain(|job_id, _| {
            self.versions_by_recency
                .iter()
                .any(|entry| entry.identity.job_id == *job_id)
        });
    }
}

pub(crate) struct CompareResultRepository {
    store: Mutex<CompareResultStore>,
}

impl Default for CompareResultRepository {
    fn default() -> Self {
        Self {
            store: Mutex::new(CompareResultStore::with_capacity(
                DEFAULT_RETAINED_VERSION_CAPACITY,
            )),
        }
    }
}

impl CompareResultRepository {
    #[cfg(test)]
    fn with_capacity(capacity: usize) -> Self {
        Self {
            store: Mutex::new(CompareResultStore::with_capacity(capacity)),
        }
    }

    pub(crate) fn insert_version(
        &self,
        version: SuccessfulCompareResult,
    ) -> Result<(), CompareResultRepositoryError> {
        self.store.lock().unwrap().insert_version(version)
    }

    pub(crate) fn get_exact(
        &self,
        identity: &CompareIdentity,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        self.store.lock().unwrap().get_exact(identity)
    }

    pub(crate) fn latest_for(
        &self,
        job_id: &str,
        target_index: usize,
        config_revision: &str,
    ) -> Result<Option<RetainedCompareResult>, CompareResultRepositoryError> {
        self.store.lock().unwrap().latest_for(&CompareScope::new(
            job_id,
            target_index,
            config_revision,
        ))
    }

    pub(crate) fn invalidate_revision(&self, job_id: &str, config_revision: &str) {
        self.store
            .lock()
            .unwrap()
            .invalidate_revision(job_id, config_revision);
    }

    pub(crate) fn invalidate_job(&self, job_id: &str) {
        self.store.lock().unwrap().invalidate_job(job_id);
    }

    pub(crate) fn rebind_job_name(&self, job_id: &str, job_name: &str) {
        self.store.lock().unwrap().rebind_job_name(job_id, job_name);
    }
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
    use syncdash::model::table::{Header, Snapshot};

    fn identity(
        job_id: &str,
        target_index: usize,
        revision: &str,
        compare_run_id: u64,
    ) -> CompareIdentity {
        CompareIdentity {
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
        let snapshot = |root: &str| Snapshot {
            header: Header {
                schema: syncdash::model::table::SCHEMA,
                kind: "snapshot".into(),
                root: root.into(),
                host: "host".into(),
                os: "test".into(),
                scanned_at_ms: 0,
                duration_ms: 0,
                entry_count: 0,
                hashed: false,
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
            format!("digest-{compare_run_id}"),
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

    #[test]
    fn exact_versions_survive_newer_publications_for_the_same_scope() {
        let repository = CompareResultRepository::with_capacity(4);
        repository
            .insert_version(version("job-a", "A", 0, "revision-a", 1))
            .unwrap();
        repository
            .insert_version(version("job-a", "A", 0, "revision-a", 2))
            .unwrap();

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
    fn reading_an_older_version_does_not_change_the_latest_pointer() {
        let repository = CompareResultRepository::with_capacity(4);
        repository
            .insert_version(version("job-a", "A", 0, "revision-a", 1))
            .unwrap();
        repository
            .insert_version(version("job-a", "A", 0, "revision-a", 2))
            .unwrap();
        repository
            .get_exact(&identity("job-a", 0, "revision-a", 1))
            .unwrap()
            .unwrap();

        assert_eq!(
            repository
                .latest_for("job-a", 0, "revision-a")
                .unwrap()
                .unwrap()
                .identity()
                .compare_run_id,
            2
        );
    }

    #[test]
    fn hot_scope_churn_does_not_evict_another_scopes_only_latest_result() {
        let repository = CompareResultRepository::with_capacity(3);
        repository
            .insert_version(version("job-a", "A", 0, "revision-a", 1))
            .unwrap();
        repository
            .insert_version(version("job-b", "B", 0, "revision-b", 2))
            .unwrap();
        repository
            .insert_version(version("job-b", "B", 0, "revision-b", 3))
            .unwrap();
        repository
            .insert_version(version("job-b", "B", 0, "revision-b", 4))
            .unwrap();
        repository
            .insert_version(version("job-b", "B", 0, "revision-b", 5))
            .unwrap();

        assert!(repository
            .get_exact(&identity("job-a", 0, "revision-a", 1))
            .unwrap()
            .is_some());
        assert_eq!(
            repository
                .latest_for("job-a", 0, "revision-a")
                .unwrap()
                .unwrap()
                .identity()
                .compare_run_id,
            1
        );
        assert_eq!(
            repository
                .latest_for("job-b", 0, "revision-b")
                .unwrap()
                .unwrap()
                .identity()
                .compare_run_id,
            5
        );
    }

    #[test]
    fn latest_pointer_disappears_when_distinct_scopes_exceed_capacity() {
        let repository = CompareResultRepository::with_capacity(2);
        repository
            .insert_version(version("job-a", "A", 0, "revision-a", 1))
            .unwrap();
        repository
            .insert_version(version("job-b", "B", 0, "revision-b", 2))
            .unwrap();
        repository
            .get_exact(&identity("job-a", 0, "revision-a", 1))
            .unwrap()
            .unwrap();
        repository
            .insert_version(version("job-c", "C", 0, "revision-c", 3))
            .unwrap();

        assert!(repository
            .latest_for("job-b", 0, "revision-b")
            .unwrap()
            .is_none());
        assert!(repository
            .latest_for("job-a", 0, "revision-a")
            .unwrap()
            .is_some());
    }

    #[test]
    fn invalidation_is_scoped_by_stable_job_identity_and_revision() {
        let repository = CompareResultRepository::with_capacity(4);
        repository
            .insert_version(version("job-a", "A", 0, "revision-old", 1))
            .unwrap();
        repository
            .insert_version(version("job-a", "A", 0, "revision-current", 2))
            .unwrap();
        repository
            .insert_version(version("job-b", "A", 0, "revision-old", 3))
            .unwrap();

        repository.invalidate_revision("job-a", "revision-old");
        assert!(repository
            .get_exact(&identity("job-a", 0, "revision-old", 1))
            .unwrap()
            .is_none());
        assert!(repository
            .get_exact(&identity("job-a", 0, "revision-current", 2))
            .unwrap()
            .is_some());
        assert!(repository
            .get_exact(&identity("job-b", 0, "revision-old", 3))
            .unwrap()
            .is_some());

        repository.invalidate_job("job-a");
        assert!(repository
            .get_exact(&identity("job-a", 0, "revision-current", 2))
            .unwrap()
            .is_none());
    }

    #[test]
    fn rename_rebinds_only_presentation_for_every_retained_version() {
        let repository = CompareResultRepository::with_capacity(4);
        repository
            .insert_version(version("job-a", "A", 0, "revision-a", 1))
            .unwrap();
        repository
            .insert_version(version("job-a", "A", 0, "revision-a", 2))
            .unwrap();

        repository.rebind_job_name("job-a", "Archive");
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
        let repository = CompareResultRepository::with_capacity(2);
        repository
            .insert_version(version("job-a", "A", 1, "revision-a", 7))
            .unwrap();
        let owner = owner("job-a", "A", 1, "revision-a", 7);
        let retained = repository.get_exact(&owner.identity).unwrap();
        assert!(validate_retained_compare(
            retained.as_ref(),
            &owner,
            "job-a",
            "A",
            1,
            "revision-a",
            Some("digest-7"),
        )
        .is_ok());

        let wrong_digest = validate_retained_compare(
            retained.as_ref(),
            &owner,
            "job-a",
            "A",
            1,
            "revision-a",
            Some("digest-8"),
        )
        .unwrap_err();
        assert!(wrong_digest.contains("no longer matches"), "{wrong_digest}");

        let missing = validate_retained_compare(None, &owner, "job-a", "A", 1, "revision-a", None)
            .unwrap_err();
        assert!(missing.contains("exact Compare result"), "{missing}");
    }

    #[test]
    fn missing_presentation_state_fails_closed_instead_of_reusing_a_stale_plan_label() {
        let repository = CompareResultRepository::with_capacity(2);
        repository
            .insert_version(version("job-a", "A", 0, "revision-a", 1))
            .unwrap();
        repository.store.lock().unwrap().job_names.remove("job-a");

        let error = match repository.get_exact(&identity("job-a", 0, "revision-a", 1)) {
            Err(error) => error,
            Ok(_) => panic!("missing presentation state must fail closed"),
        };
        assert_eq!(
            error,
            CompareResultRepositoryError::MissingJobPresentation {
                job_id: "job-a".into()
            }
        );
    }
}
