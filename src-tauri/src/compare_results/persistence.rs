//! Durable storage for immutable Compare evidence.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use syncdash::foundation::names::TEMP_PREFIX;
use syncdash::foundation::path::{RootRelativeDir, RootRelativePath};
use syncdash::fs::local_root::LocalRoot;
use syncdash::model::plan::{Action, Op, Plan, PlanHeader, PLAN_SCHEMA};
use syncdash::model::table::{ObservedEntry, TableArtifact, TableHeader, TableKind, TABLE_SCHEMA};
use syncdash::pipeline::compare::evidence::RowMeta;
use syncdash::pipeline::compare::CompareOptions;

use super::{CompareResultVersion, CompareScope};
use crate::dto::CompareIdentity;

const STORE_SCHEMA: u32 = 1;
const STORE_DIRECTORY_NAME: &str = "compare-results";
const RESULT_DIRECTORY_NAME: &str = "results";
const INDEX_FILE_NAME: &str = "index.json";
const LOCK_FILE_NAME: &str = ".compare-results.lock";
const RESULT_FILE_SUFFIX: &str = ".jsonl";
const RESULT_ID_HEX_LENGTH: usize = 32;
const DIGEST_HEX_LENGTH: usize = 64;
const INDEX_CHECKSUM_DOMAIN: &[u8] = b"syncdash-compare-index-v1\0";
const RESULT_CHECKSUM_DOMAIN: &[u8] = b"syncdash-compare-result-v1\0";

pub(super) struct LoadedCompareResults {
    pub(super) generation: u64,
    pub(super) identities_by_id: HashMap<String, CompareIdentity>,
    pub(super) latest_by_scope: HashMap<CompareScope, CompareIdentity>,
    pub(super) job_names: HashMap<String, String>,
}

pub(super) struct PersistedPublication {
    pub(super) generation: u64,
    pub(super) version: Arc<CompareResultVersion>,
}

pub(super) struct PersistedForget {
    pub(super) generation: u64,
    pub(super) cleanup_error: Option<String>,
}

pub(super) struct CompareResultPersistence {
    root: LocalRoot,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexEnvelope {
    schema: u32,
    checksum: String,
    state: IndexState,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexState {
    generation: u64,
    last_publication_sequence: u64,
    results: BTreeMap<String, IndexedResult>,
    latest_by_scope: Vec<LatestResult>,
    job_names: BTreeMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexedResult {
    identity: CompareIdentity,
    publication_sequence: u64,
    artifact_checksum: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatestResult {
    job_id: String,
    target_index: usize,
    config_revision: String,
    result_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultManifest {
    schema: u32,
    record: String,
    identity: CompareIdentity,
    plan_digest: String,
    plan_header: PlanHeader,
    compare_options: CompareOptions,
    identical_count: u64,
    identical_bytes: u64,
    operation_count: u64,
    source_header: TableHeader,
    source_entry_count: u64,
    target_header: TableHeader,
    target_entry_count: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationRecord {
    record: String,
    operation: Op,
    metadata: Option<RowMeta>,
}

#[derive(Serialize)]
struct OperationRecordRef<'a> {
    record: &'static str,
    operation: &'a Op,
    metadata: &'a Option<RowMeta>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotEntryRecord {
    record: String,
    entry: ObservedEntry,
}

#[derive(Serialize)]
struct SnapshotEntryRecordRef<'a> {
    record: &'static str,
    entry: &'a ObservedEntry,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultFooter {
    record: String,
    checksum: String,
}

impl IndexState {
    fn empty() -> Self {
        Self {
            generation: 0,
            last_publication_sequence: 0,
            results: BTreeMap::new(),
            latest_by_scope: Vec::new(),
            job_names: BTreeMap::new(),
        }
    }

    fn next_generation(&self) -> std::io::Result<u64> {
        self.generation
            .checked_add(1)
            .ok_or_else(|| invalid_data("the Compare-result repository generation is exhausted"))
    }

    fn publish(
        &self,
        identity: &CompareIdentity,
        artifact_checksum: String,
        job_name: &str,
    ) -> std::io::Result<Self> {
        if self.results.contains_key(&identity.result_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Compare result '{}' already exists", identity.result_id),
            ));
        }
        let publication_sequence = self
            .last_publication_sequence
            .checked_add(1)
            .ok_or_else(|| invalid_data("the Compare-result publication sequence is exhausted"))?;
        let mut next = self.clone();
        next.generation = self.next_generation()?;
        next.last_publication_sequence = publication_sequence;
        next.results.insert(
            identity.result_id.clone(),
            IndexedResult {
                identity: identity.clone(),
                publication_sequence,
                artifact_checksum,
            },
        );
        next.job_names
            .insert(identity.job_id.clone(), job_name.to_string());
        let scope = CompareScope::from_identity(identity);
        next.latest_by_scope
            .retain(|latest| !latest.matches(&scope));
        next.latest_by_scope
            .push(LatestResult::new(&scope, identity.result_id.clone()));
        next.latest_by_scope.sort_by(compare_latest_scope);
        validate_index_state(&next)?;
        Ok(next)
    }

    fn rebind_job_name(&self, job_id: &str, job_name: &str) -> std::io::Result<Option<Self>> {
        let Some(current_name) = self.job_names.get(job_id) else {
            return Ok(None);
        };
        if current_name == job_name {
            return Ok(None);
        }
        let mut next = self.clone();
        next.generation = self.next_generation()?;
        next.job_names
            .insert(job_id.to_string(), job_name.to_string());
        validate_index_state(&next)?;
        Ok(Some(next))
    }

    fn forget(&self, identity: &CompareIdentity) -> std::io::Result<Self> {
        let indexed = self.results.get(&identity.result_id).ok_or_else(|| {
            invalid_data(format!(
                "Compare result '{}' is missing from the authoritative index",
                identity.result_id
            ))
        })?;
        if indexed.identity != *identity {
            return Err(invalid_data(format!(
                "Compare result ID '{}' belongs to a different immutable identity",
                identity.result_id
            )));
        }
        let mut next = self.clone();
        next.generation = self.next_generation()?;
        next.results.remove(&identity.result_id);
        next.latest_by_scope
            .retain(|latest| latest.result_id != identity.result_id);
        if !next
            .results
            .values()
            .any(|result| result.identity.job_id == identity.job_id)
        {
            next.job_names.remove(&identity.job_id);
        }
        validate_index_state(&next)?;
        Ok(next)
    }
}

impl LatestResult {
    fn new(scope: &CompareScope, result_id: String) -> Self {
        Self {
            job_id: scope.job_id.clone(),
            target_index: scope.target_index,
            config_revision: scope.config_revision.clone(),
            result_id,
        }
    }

    fn scope(&self) -> CompareScope {
        CompareScope::new(&self.job_id, self.target_index, &self.config_revision)
    }

    fn matches(&self, scope: &CompareScope) -> bool {
        self.job_id == scope.job_id
            && self.target_index == scope.target_index
            && self.config_revision == scope.config_revision
    }
}

impl CompareResultPersistence {
    pub(super) fn open_default() -> std::io::Result<(Self, LoadedCompareResults)> {
        Self::open_at(syncdash::foundation::dirs::data_dir().join(STORE_DIRECTORY_NAME))
    }

    pub(super) fn open_at(path: PathBuf) -> std::io::Result<(Self, LoadedCompareResults)> {
        let persistence = Self {
            root: LocalRoot::create(path)?,
        };
        persistence.root.create_directory_all(&result_directory())?;
        let loaded = persistence.with_lock(|this| this.load_locked())?;
        Ok((persistence, loaded))
    }

    pub(super) fn publish(
        &self,
        expected_generation: u64,
        version: CompareResultVersion,
        job_name: &str,
    ) -> std::io::Result<PersistedPublication> {
        self.with_lock(|this| {
            let current = this.read_index_locked()?;
            require_generation(&current.state, expected_generation)?;
            validate_compare_result(&version)?;
            let artifact_checksum = calculate_result_checksum(&version)?;
            let next = current
                .state
                .publish(&version.identity, artifact_checksum.clone(), job_name)?;
            let next_envelope = index_envelope(next)?;
            let artifact_path = result_path(&version.identity.result_id)?;
            write_result_artifact(
                &this.root,
                &artifact_path,
                &version,
                &artifact_checksum,
            )?;
            let opened_artifact = this.root.open_read(&artifact_path)?;
            if let Err(error) = this.write_index_locked(&next_envelope) {
                if this
                    .read_index_locked()
                    .is_ok_and(|visible| {
                        visible.state.generation == next_envelope.state.generation
                            && visible.checksum == next_envelope.checksum
                    })
                {
                    return Ok(PersistedPublication {
                        generation: next_envelope.state.generation,
                        version: Arc::new(version),
                    });
                }
                let rollback = this
                    .root
                    .remove_open_file(&artifact_path, &opened_artifact)
                    .and_then(|()| this.root.sync_parent(&artifact_path));
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(std::io::Error::other(format!(
                        "Compare index publication failed ({error}); immutable artifact rollback also failed ({rollback_error})"
                    ))),
                };
            }
            Ok(PersistedPublication {
                generation: next_envelope.state.generation,
                version: Arc::new(version),
            })
        })
    }

    pub(super) fn rebind_job_name(
        &self,
        expected_generation: u64,
        job_id: &str,
        job_name: &str,
    ) -> std::io::Result<Option<u64>> {
        self.with_lock(|this| {
            let current = this.read_index_locked()?;
            require_generation(&current.state, expected_generation)?;
            let Some(next) = current.state.rebind_job_name(job_id, job_name)? else {
                return Ok(None);
            };
            let next = index_envelope(next)?;
            this.write_index_locked(&next)?;
            Ok(Some(next.state.generation))
        })
    }

    pub(super) fn forget(
        &self,
        expected_generation: u64,
        identity: &CompareIdentity,
    ) -> std::io::Result<PersistedForget> {
        self.with_lock(|this| {
            let current = this.read_index_locked()?;
            require_generation(&current.state, expected_generation)?;
            let next_state = current.state.forget(identity)?;
            let artifact_path = result_path(&identity.result_id)?;
            let artifact_file = this.root.open_read(&artifact_path)?;
            this.read_result_locked(identity, None)?;
            let next = index_envelope(next_state)?;
            this.write_index_locked(&next)?;
            let cleanup_error = this
                .root
                .remove_open_file(&artifact_path, &artifact_file)
                .and_then(|()| this.root.sync_parent(&artifact_path))
                .err()
                .map(|error| {
                    format!(
                        "Compare result was forgotten, but its unindexed artifact could not be removed; startup cleanup will retry: {error}"
                    )
                });
            Ok(PersistedForget {
                generation: next.state.generation,
                cleanup_error,
            })
        })
    }

    pub(super) fn load_exact(
        &self,
        expected_generation: u64,
        identity: &CompareIdentity,
    ) -> std::io::Result<Arc<CompareResultVersion>> {
        self.with_lock(|this| {
            let index = this.read_index_locked()?;
            require_generation(&index.state, expected_generation)?;
            let indexed = index
                .state
                .results
                .get(&identity.result_id)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("Compare result '{}' is not retained", identity.result_id),
                    )
                })?;
            if indexed.identity != *identity {
                return Err(invalid_data(format!(
                    "Compare result ID '{}' belongs to a different immutable identity",
                    identity.result_id
                )));
            }
            this.read_result_locked(identity, Some(&indexed.artifact_checksum))
                .map(Arc::new)
        })
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce(&Self) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        let lock = self.root.open_lock_file(&lock_path())?;
        lock.lock()?;
        self.clean_staging_files_locked()?;
        self.ensure_index_locked()?;
        self.clean_unindexed_artifacts_locked()?;
        operation(self)
    }

    fn ensure_index_locked(&self) -> std::io::Result<()> {
        match self.root.open_read(&index_path()) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !self
                    .root
                    .read_directory_names(&result_directory())?
                    .is_empty()
                {
                    return Err(invalid_data(
                        "Compare-result artifacts exist without their authoritative index",
                    ));
                }
                let envelope = index_envelope(IndexState::empty())?;
                match write_staged(&self.root, &index_path(), &serialize_json(&envelope)?, true) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        self.root.open_read(&index_path()).map(drop)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn load_locked(&self) -> std::io::Result<LoadedCompareResults> {
        let index = self.read_index_locked()?;
        self.validate_root_inventory_locked()?;
        for indexed in index.state.results.values() {
            self.read_result_locked(&indexed.identity, Some(indexed.artifact_checksum.as_str()))?;
        }
        let latest_by_scope = index
            .state
            .latest_by_scope
            .iter()
            .map(|latest| {
                let indexed = index
                    .state
                    .results
                    .get(&latest.result_id)
                    .expect("validated latest pointers reference an indexed result");
                (latest.scope(), indexed.identity.clone())
            })
            .collect();
        Ok(LoadedCompareResults {
            generation: index.state.generation,
            identities_by_id: index
                .state
                .results
                .values()
                .map(|indexed| (indexed.identity.result_id.clone(), indexed.identity.clone()))
                .collect(),
            latest_by_scope,
            job_names: index.state.job_names.into_iter().collect(),
        })
    }

    fn read_index_locked(&self) -> std::io::Result<IndexEnvelope> {
        let bytes = self.root.read(&index_path())?;
        let envelope: IndexEnvelope = serde_json::from_slice(&bytes)
            .map_err(|error| invalid_data(format!("invalid Compare-result index JSON: {error}")))?;
        if serialize_json(&envelope)? != bytes {
            return Err(invalid_data(
                "Compare-result index contains non-canonical or unknown fields",
            ));
        }
        validate_index_envelope(&envelope)?;
        Ok(envelope)
    }

    fn write_index_locked(&self, envelope: &IndexEnvelope) -> std::io::Result<()> {
        validate_index_envelope(envelope)?;
        match write_staged(&self.root, &index_path(), &serialize_json(envelope)?, false) {
            Ok(()) => Ok(()),
            Err(_)
                if self.read_index_locked().is_ok_and(|visible| {
                    visible.state.generation == envelope.state.generation
                        && visible.checksum == envelope.checksum
                }) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn read_result_locked(
        &self,
        identity: &CompareIdentity,
        expected_checksum: Option<&str>,
    ) -> std::io::Result<CompareResultVersion> {
        let result = read_result_artifact(
            &self.root,
            &result_path(&identity.result_id)?,
            expected_checksum,
        )?;
        if result.identity != *identity {
            return Err(invalid_data(format!(
                "Compare artifact '{}' does not match its indexed identity",
                identity.result_id
            )));
        }
        Ok(result)
    }

    fn clean_staging_files_locked(&self) -> std::io::Result<()> {
        self.clean_staging_in_directory(&root_directory(), "")?;
        self.clean_staging_in_directory(&result_directory(), RESULT_DIRECTORY_NAME)
    }

    fn clean_unindexed_artifacts_locked(&self) -> std::io::Result<()> {
        let index = self.read_index_locked()?;
        let indexed_names = index
            .state
            .results
            .keys()
            .map(|result_id| result_file_name(result_id))
            .collect::<HashSet<_>>();
        for name in self.root.read_directory_names(&result_directory())? {
            if indexed_names.contains(name.as_str()) {
                continue;
            }
            let Some(result_id) = parse_result_file_name(name.as_str()) else {
                return Err(invalid_data(format!(
                    "unexpected entry '{}' in the Compare-result artifact directory",
                    name.as_str()
                )));
            };
            let path = result_path(result_id)?;
            let opened = self.root.open_read(&path)?;
            let orphan = read_result_artifact(&self.root, &path, None)?;
            if orphan.identity.result_id != result_id {
                return Err(invalid_data(format!(
                    "uncommitted Compare artifact '{result_id}' contains result '{}'",
                    orphan.identity.result_id
                )));
            }
            self.root.remove_open_file(&path, &opened)?;
            self.root.sync_parent(&path)?;
        }
        Ok(())
    }

    fn clean_staging_in_directory(
        &self,
        directory: &RootRelativeDir,
        prefix: &str,
    ) -> std::io::Result<()> {
        for name in self.root.read_directory_names(directory)? {
            if !name.as_str().starts_with(TEMP_PREFIX) {
                continue;
            }
            let relative = if prefix.is_empty() {
                name.as_str().to_string()
            } else {
                format!("{prefix}/{}", name.as_str())
            };
            let relative = RootRelativePath::new(relative)
                .expect("typed directory and entry names compose into a relative path");
            let opened = self.root.open_read(&relative)?;
            self.root.remove_open_file(&relative, &opened)?;
            self.root.sync_parent(&relative)?;
        }
        Ok(())
    }

    fn validate_root_inventory_locked(&self) -> std::io::Result<()> {
        for name in self.root.read_directory_names(&root_directory())? {
            match name.as_str() {
                INDEX_FILE_NAME | LOCK_FILE_NAME | RESULT_DIRECTORY_NAME => {}
                unexpected => {
                    return Err(invalid_data(format!(
                        "unexpected entry '{unexpected}' in the Compare-result store"
                    )))
                }
            }
        }
        Ok(())
    }
}

fn validate_index_envelope(envelope: &IndexEnvelope) -> std::io::Result<()> {
    require_schema(envelope.schema, "Compare-result index")?;
    validate_digest(&envelope.checksum, "Compare-result index checksum")?;
    let expected = checksum(INDEX_CHECKSUM_DOMAIN, &envelope.state)?;
    if envelope.checksum != expected {
        return Err(invalid_data("the Compare-result index checksum is invalid"));
    }
    validate_index_state(&envelope.state)
}

fn validate_index_state(state: &IndexState) -> std::io::Result<()> {
    if state.generation < state.last_publication_sequence {
        return Err(invalid_data(
            "Compare-result index generation precedes its publication sequence",
        ));
    }
    let mut publication_sequences = HashSet::with_capacity(state.results.len());
    let mut maximum_sequence = 0_u64;
    let mut referenced_jobs = HashSet::new();
    for (result_id, indexed) in &state.results {
        validate_result_id(result_id)?;
        validate_identity(&indexed.identity)?;
        if indexed.identity.result_id != *result_id {
            return Err(invalid_data(format!(
                "Compare index key '{result_id}' does not match its immutable identity"
            )));
        }
        if indexed.publication_sequence == 0
            || !publication_sequences.insert(indexed.publication_sequence)
        {
            return Err(invalid_data(
                "Compare-result publication sequences must be unique and non-zero",
            ));
        }
        maximum_sequence = maximum_sequence.max(indexed.publication_sequence);
        validate_digest(
            &indexed.artifact_checksum,
            "Compare-result artifact checksum",
        )?;
        referenced_jobs.insert(indexed.identity.job_id.clone());
    }
    if maximum_sequence > state.last_publication_sequence {
        return Err(invalid_data(
            "Compare-result index publication sequence precedes a retained record",
        ));
    }
    let presented_jobs = state.job_names.keys().cloned().collect::<HashSet<_>>();
    if referenced_jobs != presented_jobs
        || state.job_names.values().any(|name| name.trim().is_empty())
    {
        return Err(invalid_data(
            "Compare-result presentation names do not exactly cover retained job identities",
        ));
    }
    let mut previous: Option<&LatestResult> = None;
    let mut scopes = HashSet::with_capacity(state.latest_by_scope.len());
    for latest in &state.latest_by_scope {
        validate_result_id(&latest.result_id)?;
        if previous.is_some_and(|prior| compare_latest_scope(prior, latest) != Ordering::Less) {
            return Err(invalid_data(
                "Compare-result latest pointers are not uniquely ordered by scope",
            ));
        }
        previous = Some(latest);
        let scope = latest.scope();
        if !scopes.insert(scope.clone()) {
            return Err(invalid_data("duplicate Compare-result latest scope"));
        }
        let indexed = state.results.get(&latest.result_id).ok_or_else(|| {
            invalid_data(format!(
                "Compare latest pointer '{}' has no indexed artifact",
                latest.result_id
            ))
        })?;
        if !scope.contains(&indexed.identity) {
            return Err(invalid_data(format!(
                "Compare latest pointer '{}' crosses result scopes",
                latest.result_id
            )));
        }
        let newest_sequence = state
            .results
            .values()
            .filter(|candidate| scope.contains(&candidate.identity))
            .map(|candidate| candidate.publication_sequence)
            .max()
            .expect("a latest pointer references at least one result in its scope");
        if newest_sequence != indexed.publication_sequence {
            return Err(invalid_data(format!(
                "Compare latest pointer '{}' is not the newest retained publication for its scope",
                latest.result_id
            )));
        }
    }
    Ok(())
}

fn validate_compare_result(version: &CompareResultVersion) -> std::io::Result<()> {
    validate_identity(&version.identity)?;
    validate_digest(&version.plan_digest, "Compare plan digest")?;
    let plan_header = &version.plan.header;
    let operations = &version.plan.operations;
    if Plan::digest_parts(plan_header, operations) != version.plan_digest {
        return Err(invalid_data(format!(
            "Compare artifact '{}' plan digest is invalid",
            version.identity.result_id
        )));
    }
    if plan_header.schema != PLAN_SCHEMA || plan_header.kind != "plan" {
        return Err(invalid_data(
            "retained Compare plan has an unsupported schema or kind",
        ));
    }
    if plan_header.op_count != operations.len() as u64
        || plan_header.conflict_count
            != operations
                .iter()
                .filter(|operation| matches!(operation.action, Action::Conflict))
                .count() as u64
        || version.plan.metadata.len() != operations.len()
    {
        return Err(invalid_data(
            "retained Compare plan counts do not match its operations and metadata",
        ));
    }
    validate_snapshot("source", &version.source)?;
    validate_snapshot("target", &version.target)?;
    if plan_header.source_root != version.source.header.root
        || plan_header.source_host != version.source.header.host
        || plan_header.target_root != version.target.header.root
        || plan_header.target_host != version.target.header.host
        || plan_header.source_entries != version.source.entries.len() as u64
        || plan_header.target_entries != version.target.entries.len() as u64
        || plan_header.source_excluded
            != version.source.header.excluded_dirs + version.source.header.excluded_files
        || plan_header.target_excluded
            != version.target.header.excluded_dirs + version.target.header.excluded_files
        || plan_header.source_walk_errors != version.source.header.walk_errors
        || plan_header.target_walk_errors != version.target.header.walk_errors
        || plan_header.source_walk_err_samples != version.source.header.walk_err_samples
        || plan_header.target_walk_err_samples != version.target.header.walk_err_samples
        || plan_header.source_icloud_stubs != version.source.header.icloud_stubs
        || plan_header.target_icloud_stubs != version.target.header.icloud_stubs
        || plan_header.source_icloud_stub_samples != version.source.header.icloud_stub_samples
        || plan_header.target_icloud_stub_samples != version.target.header.icloud_stub_samples
    {
        return Err(invalid_data(
            "retained Compare plan header does not attest to its exact snapshots",
        ));
    }
    if version.compare_options.max_conflicts < -1 || version.compare_options.mtime_window_ms < 0 {
        return Err(invalid_data(
            "retained Compare options are outside their valid range",
        ));
    }
    let evidence = syncdash::pipeline::compare::evidence::evidence_for_operations(
        &version.source,
        &version.target,
        operations,
        &version.compare_options,
    );
    if evidence.identical_count != version.plan.identical_count
        || evidence.identical_bytes != version.plan.identical_bytes
    {
        return Err(invalid_data(
            "retained Compare identical-item totals do not match its snapshots",
        ));
    }
    for ((retained, derived), operation) in version
        .plan
        .metadata
        .iter()
        .zip(evidence.metas.iter())
        .zip(operations.iter())
    {
        match retained {
            Some(retained) if retained == derived => {}
            None if matches!(operation.action, Action::Copy)
                && operation.size.is_some()
                && operation.mtime_ms.is_some() => {}
            _ => {
                return Err(invalid_data(
                    "retained Compare row metadata does not match its snapshots",
                ))
            }
        }
    }
    Ok(())
}

fn validate_snapshot(side: &str, snapshot: &TableArtifact) -> std::io::Result<()> {
    if snapshot.header.schema != TABLE_SCHEMA || snapshot.header.kind != TableKind::Snapshot {
        return Err(invalid_data(format!(
            "retained {side} snapshot has an unsupported schema or kind"
        )));
    }
    if snapshot.header.entry_count != snapshot.entries.len() as u64 {
        return Err(invalid_data(format!(
            "retained {side} snapshot entry count is invalid"
        )));
    }
    snapshot
        .validate()
        .map_err(|error| invalid_data(format!("retained {side} snapshot is invalid: {error}")))
}

fn validate_identity(identity: &CompareIdentity) -> std::io::Result<()> {
    validate_result_id(&identity.result_id)?;
    if identity.job_id.trim().is_empty() || identity.config_revision.trim().is_empty() {
        return Err(invalid_data(
            "Compare identity has an empty job ID or configuration revision",
        ));
    }
    Ok(())
}

fn validate_result_id(result_id: &str) -> std::io::Result<()> {
    if result_id.len() != RESULT_ID_HEX_LENGTH
        || !result_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_data(format!(
            "Compare result ID '{result_id}' is not {RESULT_ID_HEX_LENGTH} lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

fn validate_digest(digest: &str, label: &str) -> std::io::Result<()> {
    if digest.len() != DIGEST_HEX_LENGTH
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_data(format!(
            "{label} is not {DIGEST_HEX_LENGTH} lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

fn require_generation(state: &IndexState, expected: u64) -> std::io::Result<()> {
    if state.generation != expected {
        return Err(std::io::Error::other(format!(
            "Compare-result repository changed in another process (expected generation {expected}, found {}) — restart SyncDash before changing retained results",
            state.generation
        )));
    }
    Ok(())
}

fn require_schema(actual: u32, artifact: &str) -> std::io::Result<()> {
    if actual != STORE_SCHEMA {
        return Err(invalid_data(format!(
            "{artifact} uses schema {actual}; this build requires schema {STORE_SCHEMA}"
        )));
    }
    Ok(())
}

fn index_envelope(state: IndexState) -> std::io::Result<IndexEnvelope> {
    validate_index_state(&state)?;
    Ok(IndexEnvelope {
        schema: STORE_SCHEMA,
        checksum: checksum(INDEX_CHECKSUM_DOMAIN, &state)?,
        state,
    })
}

fn checksum<T: Serialize>(domain: &[u8], value: &T) -> std::io::Result<String> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        invalid_data(format!(
            "cannot encode Compare-result checksum input: {error}"
        ))
    })?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&bytes);
    Ok(hasher.finalize().to_hex().to_string())
}

fn calculate_result_checksum(version: &CompareResultVersion) -> std::io::Result<String> {
    write_result_body(&mut std::io::sink(), version)
}

fn write_result_artifact(
    root: &LocalRoot,
    relative: &RootRelativePath,
    version: &CompareResultVersion,
    expected_checksum: &str,
) -> std::io::Result<()> {
    let mut staged = root.create_staged(relative)?;
    let checksum = write_result_body(&mut staged, version)?;
    if checksum != expected_checksum {
        return Err(invalid_data(
            "Compare-result streaming checksum changed between validation and publication",
        ));
    }
    write_json_line(
        &mut staged,
        &ResultFooter {
            record: "checksum".to_string(),
            checksum,
        },
    )?;
    staged.seal(true)?;
    staged.commit_noreplace()
}

fn write_result_body(
    writer: &mut impl Write,
    version: &CompareResultVersion,
) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RESULT_CHECKSUM_DOMAIN);
    let manifest = ResultManifest {
        schema: STORE_SCHEMA,
        record: "manifest".to_string(),
        identity: version.identity.clone(),
        plan_digest: version.plan_digest.clone(),
        plan_header: version.plan.header.clone(),
        compare_options: version.compare_options,
        identical_count: version.plan.identical_count,
        identical_bytes: version.plan.identical_bytes,
        operation_count: version.plan.operations.len() as u64,
        source_header: version.source.header.clone(),
        source_entry_count: version.source.entries.len() as u64,
        target_header: version.target.header.clone(),
        target_entry_count: version.target.entries.len() as u64,
    };
    write_hashed_json_line(writer, &mut hasher, &manifest)?;
    for (operation, metadata) in version
        .plan
        .operations
        .iter()
        .zip(version.plan.metadata.iter())
    {
        write_hashed_json_line(
            writer,
            &mut hasher,
            &OperationRecordRef {
                record: "operation",
                operation,
                metadata,
            },
        )?;
    }
    for entry in &version.source.entries {
        write_hashed_json_line(
            writer,
            &mut hasher,
            &SnapshotEntryRecordRef {
                record: "source_entry",
                entry,
            },
        )?;
    }
    for entry in &version.target.entries {
        write_hashed_json_line(
            writer,
            &mut hasher,
            &SnapshotEntryRecordRef {
                record: "target_entry",
                entry,
            },
        )?;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn read_result_artifact(
    root: &LocalRoot,
    relative: &RootRelativePath,
    expected_checksum: Option<&str>,
) -> std::io::Result<CompareResultVersion> {
    let file = root.open_read(relative)?;
    let artifact_size = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    hasher.update(RESULT_CHECKSUM_DOMAIN);
    let manifest: ResultManifest = read_json_line(&mut reader, Some(&mut hasher), relative)?;
    require_schema(manifest.schema, "Compare-result artifact")?;
    require_record(&manifest.record, "manifest", relative)?;
    let operation_count = checked_record_count(
        manifest.operation_count,
        artifact_size,
        "operation",
        relative,
    )?;
    let source_entry_count = checked_record_count(
        manifest.source_entry_count,
        artifact_size,
        "source entry",
        relative,
    )?;
    let target_entry_count = checked_record_count(
        manifest.target_entry_count,
        artifact_size,
        "target entry",
        relative,
    )?;
    let mut operations = Vec::with_capacity(operation_count);
    let mut metadata = Vec::with_capacity(operation_count);
    for _ in 0..operation_count {
        let record: OperationRecord = read_json_line(&mut reader, Some(&mut hasher), relative)?;
        require_record(&record.record, "operation", relative)?;
        operations.push(record.operation);
        metadata.push(record.metadata);
    }
    let mut source_entries = Vec::with_capacity(source_entry_count);
    for _ in 0..source_entry_count {
        let record: SnapshotEntryRecord = read_json_line(&mut reader, Some(&mut hasher), relative)?;
        require_record(&record.record, "source_entry", relative)?;
        source_entries.push(record.entry);
    }
    let mut target_entries = Vec::with_capacity(target_entry_count);
    for _ in 0..target_entry_count {
        let record: SnapshotEntryRecord = read_json_line(&mut reader, Some(&mut hasher), relative)?;
        require_record(&record.record, "target_entry", relative)?;
        target_entries.push(record.entry);
    }
    let footer: ResultFooter = read_json_line(&mut reader, None, relative)?;
    require_record(&footer.record, "checksum", relative)?;
    validate_digest(&footer.checksum, "Compare-result artifact checksum")?;
    let computed_checksum = hasher.finalize().to_hex().to_string();
    if footer.checksum != computed_checksum {
        return Err(invalid_data(format!(
            "Compare artifact '{}' checksum is invalid",
            relative.as_str()
        )));
    }
    if expected_checksum.is_some_and(|expected| expected != computed_checksum) {
        return Err(invalid_data(format!(
            "Compare artifact '{}' checksum does not match its index record",
            relative.as_str()
        )));
    }
    let mut trailing = Vec::new();
    if reader.read_until(b'\n', &mut trailing)? != 0 {
        return Err(invalid_data(format!(
            "Compare artifact '{}' has records after its checksum footer",
            relative.as_str()
        )));
    }
    let version = CompareResultVersion {
        identity: manifest.identity,
        plan_digest: manifest.plan_digest,
        plan: super::RetainedPlan {
            header: manifest.plan_header,
            operations,
            metadata,
            identical_count: manifest.identical_count,
            identical_bytes: manifest.identical_bytes,
        },
        source: TableArtifact {
            header: manifest.source_header,
            entries: source_entries,
        },
        target: TableArtifact {
            header: manifest.target_header,
            entries: target_entries,
        },
        compare_options: manifest.compare_options,
    };
    validate_compare_result(&version)?;
    Ok(version)
}

fn write_hashed_json_line(
    writer: &mut impl Write,
    hasher: &mut blake3::Hasher,
    value: &impl Serialize,
) -> std::io::Result<()> {
    let mut hashing_writer = HashingWriter { writer, hasher };
    write_json_line(&mut hashing_writer, value)
}

fn write_json_line(writer: &mut impl Write, value: &impl Serialize) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)
        .map_err(|error| invalid_data(format!("cannot encode Compare-result record: {error}")))?;
    writer.write_all(b"\n")
}

fn read_json_line<T: for<'de> Deserialize<'de> + Serialize>(
    reader: &mut impl BufRead,
    hasher: Option<&mut blake3::Hasher>,
    relative: &RootRelativePath,
) -> std::io::Result<T> {
    let mut line = Vec::new();
    if reader.read_until(b'\n', &mut line)? == 0 {
        return Err(invalid_data(format!(
            "Compare artifact '{}' ended before all declared records were read",
            relative.as_str()
        )));
    }
    if !line.ends_with(b"\n") {
        return Err(invalid_data(format!(
            "Compare artifact '{}' contains an unterminated record",
            relative.as_str()
        )));
    }
    if let Some(hasher) = hasher {
        hasher.update(&line);
    }
    let value: T = serde_json::from_slice(&line).map_err(|error| {
        invalid_data(format!(
            "invalid Compare-result record in '{}': {error}",
            relative.as_str()
        ))
    })?;
    let mut canonical = serde_json::to_vec(&value).map_err(|error| {
        invalid_data(format!(
            "cannot canonicalize Compare-result record in '{}': {error}",
            relative.as_str()
        ))
    })?;
    canonical.push(b'\n');
    if canonical != line {
        return Err(invalid_data(format!(
            "Compare artifact '{}' contains a non-canonical or unknown record field",
            relative.as_str()
        )));
    }
    Ok(value)
}

fn checked_record_count(
    count: u64,
    artifact_size: u64,
    label: &str,
    relative: &RootRelativePath,
) -> std::io::Result<usize> {
    if count > artifact_size {
        return Err(invalid_data(format!(
            "Compare artifact '{}' declares more {label} records than its byte length can contain",
            relative.as_str()
        )));
    }
    usize::try_from(count).map_err(|_| {
        invalid_data(format!(
            "Compare artifact '{}' declares too many {label} records for this platform",
            relative.as_str()
        ))
    })
}

fn require_record(
    actual: &str,
    expected: &str,
    relative: &RootRelativePath,
) -> std::io::Result<()> {
    if actual != expected {
        return Err(invalid_data(format!(
            "Compare artifact '{}' contains record '{actual}' where '{expected}' is required",
            relative.as_str()
        )));
    }
    Ok(())
}

struct HashingWriter<'a, W> {
    writer: &'a mut W,
    hasher: &'a mut blake3::Hasher,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.writer.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn serialize_json(value: &impl Serialize) -> std::io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| invalid_data(format!("cannot encode Compare-result JSON: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_staged(
    root: &LocalRoot,
    relative: &RootRelativePath,
    bytes: &[u8],
    no_replace: bool,
) -> std::io::Result<()> {
    let mut staged = root.create_staged(relative)?;
    staged.write_all(bytes)?;
    staged.seal(true)?;
    if no_replace {
        staged.commit_noreplace()
    } else {
        staged.commit()
    }
}

fn compare_latest_scope(left: &LatestResult, right: &LatestResult) -> Ordering {
    (&left.job_id, left.target_index, &left.config_revision).cmp(&(
        &right.job_id,
        right.target_index,
        &right.config_revision,
    ))
}

fn parse_result_file_name(name: &str) -> Option<&str> {
    name.strip_suffix(RESULT_FILE_SUFFIX)
        .filter(|result_id| validate_result_id(result_id).is_ok())
}

fn result_file_name(result_id: &str) -> String {
    format!("{result_id}{RESULT_FILE_SUFFIX}")
}

fn result_path(result_id: &str) -> std::io::Result<RootRelativePath> {
    validate_result_id(result_id)?;
    RootRelativePath::new(format!(
        "{RESULT_DIRECTORY_NAME}/{}",
        result_file_name(result_id)
    ))
    .map_err(|error| invalid_data(error.to_string()))
}

fn root_directory() -> RootRelativeDir {
    RootRelativeDir::new("").expect("the empty relative directory names the root")
}

fn result_directory() -> RootRelativeDir {
    RootRelativeDir::new(RESULT_DIRECTORY_NAME).expect("constant result directory is valid")
}

fn index_path() -> RootRelativePath {
    RootRelativePath::new(INDEX_FILE_NAME).expect("constant index name is valid")
}

fn lock_path() -> RootRelativePath {
    RootRelativePath::new(LOCK_FILE_NAME).expect("constant lock name is valid")
}

fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}
