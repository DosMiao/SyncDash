//! Locked persistence coordinator and crash-safe publication protocol.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use syncdash::foundation::names::TEMP_PREFIX;
use syncdash::foundation::path::{RootRelativeDir, RootRelativePath};
use syncdash::fs::local_root::LocalRoot;

use crate::contracts::compare::CompareIdentity;

use super::super::model::result::CompareResultVersion;
use super::artifact_codec::{read_result_artifact, write_result_artifact};
use super::artifact_validation::validate_compare_result;
use super::error::invalid_data;
use super::index_validation::validate_index_envelope;
use super::integrity::{calculate_result_checksum, index_envelope, require_generation};
use super::io::{serialize_json, write_staged};
use super::paths::{
    index_path, lock_path, parse_result_file_name, result_directory, result_file_name, result_path,
    root_directory,
};
use super::schema::{IndexEnvelope, IndexState};
use super::{
    CompareResultPersistence, LoadedCompareResults, PersistedForget, PersistedPublication,
    INDEX_FILE_NAME, LOCK_FILE_NAME, RESULT_DIRECTORY_NAME, STORE_DIRECTORY_NAME,
};

impl CompareResultPersistence {
    pub(in crate::features::compare::evidence) fn open_default(
    ) -> std::io::Result<(Self, LoadedCompareResults)> {
        Self::open_at(syncdash::foundation::dirs::data_dir().join(STORE_DIRECTORY_NAME))
    }

    pub(in crate::features::compare::evidence) fn open_at(
        path: PathBuf,
    ) -> std::io::Result<(Self, LoadedCompareResults)> {
        let persistence = Self {
            root: LocalRoot::create(path)?,
        };
        persistence.root.create_directory_all(&result_directory())?;
        let loaded = persistence.with_lock(|this| this.load_locked())?;
        Ok((persistence, loaded))
    }

    pub(in crate::features::compare::evidence) fn publish(
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

    pub(in crate::features::compare::evidence) fn rebind_job_name(
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

    pub(in crate::features::compare::evidence) fn forget(
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

    pub(in crate::features::compare::evidence) fn load_exact(
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
