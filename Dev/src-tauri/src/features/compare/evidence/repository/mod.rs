//! Serialized repository facade coordinating execution state and durable evidence.

mod error;
mod exact;
mod mutation;
mod publication;
mod query;
mod storage;
pub(crate) mod validation;
mod verification;

#[cfg(test)]
mod tests;

use std::sync::Mutex;

use super::model::error::CompareResultRepositoryError;
use super::persistence::CompareResultPersistence;
use super::state::CompareResultStore;

use self::error::storage_error;

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
}
