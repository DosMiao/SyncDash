//! Durable storage for immutable Compare evidence.

mod artifact_codec;
mod artifact_validation;
mod error;
mod index_order;
mod index_state;
mod index_validation;
mod integrity;
mod io;
mod paths;
mod repository;
mod schema;

use std::collections::HashMap;
use std::sync::Arc;

use syncdash::fs::local_root::LocalRoot;

use super::model::result::CompareResultVersion;
use super::model::scope::CompareScope;
use crate::contracts::compare::CompareIdentity;

pub(super) const STORE_SCHEMA: u32 = 1;
pub(super) const STORE_DIRECTORY_NAME: &str = "compare-results";
pub(super) const RESULT_DIRECTORY_NAME: &str = "results";
pub(super) const INDEX_FILE_NAME: &str = "index.json";
pub(super) const LOCK_FILE_NAME: &str = ".compare-results.lock";
pub(super) const RESULT_FILE_SUFFIX: &str = ".jsonl";
pub(super) const RESULT_ID_HEX_LENGTH: usize = 32;
pub(super) const DIGEST_HEX_LENGTH: usize = 64;
pub(super) const INDEX_CHECKSUM_DOMAIN: &[u8] = b"syncdash-compare-index-v1\0";
pub(super) const RESULT_CHECKSUM_DOMAIN: &[u8] = b"syncdash-compare-result-v1\0";

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
    pub(super) root: LocalRoot,
}
