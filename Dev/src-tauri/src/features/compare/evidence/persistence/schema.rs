//! Persisted index and JSONL record schema. Field order is checksum-significant.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use syncdash::model::plan::{Op, PlanHeader};
use syncdash::model::table::{ObservedEntry, TableHeader};
use syncdash::pipeline::compare::evidence::RowMeta;
use syncdash::pipeline::compare::CompareOptions;

use crate::contracts::compare::CompareIdentity;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IndexEnvelope {
    pub(super) schema: u32,
    pub(super) checksum: String,
    pub(super) state: IndexState,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IndexState {
    pub(super) generation: u64,
    pub(super) last_publication_sequence: u64,
    pub(super) results: BTreeMap<String, IndexedResult>,
    pub(super) latest_by_scope: Vec<LatestResult>,
    pub(super) job_names: BTreeMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IndexedResult {
    pub(super) identity: CompareIdentity,
    pub(super) publication_sequence: u64,
    pub(super) artifact_checksum: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LatestResult {
    pub(super) job_id: String,
    pub(super) target_index: usize,
    pub(super) config_revision: String,
    pub(super) result_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResultManifest {
    pub(super) schema: u32,
    pub(super) record: String,
    pub(super) identity: CompareIdentity,
    pub(super) plan_digest: String,
    pub(super) plan_header: PlanHeader,
    pub(super) compare_options: CompareOptions,
    pub(super) identical_count: u64,
    pub(super) identical_bytes: u64,
    pub(super) operation_count: u64,
    pub(super) source_header: TableHeader,
    pub(super) source_entry_count: u64,
    pub(super) target_header: TableHeader,
    pub(super) target_entry_count: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OperationRecord {
    pub(super) record: String,
    pub(super) operation: Op,
    pub(super) metadata: Option<RowMeta>,
}

#[derive(Serialize)]
pub(super) struct OperationRecordRef<'a> {
    pub(super) record: &'static str,
    pub(super) operation: &'a Op,
    pub(super) metadata: &'a Option<RowMeta>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SnapshotEntryRecord {
    pub(super) record: String,
    pub(super) entry: ObservedEntry,
}

#[derive(Serialize)]
pub(super) struct SnapshotEntryRecordRef<'a> {
    pub(super) record: &'static str,
    pub(super) entry: &'a ObservedEntry,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResultFooter {
    pub(super) record: String,
    pub(super) checksum: String,
}
