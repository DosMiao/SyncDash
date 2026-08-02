//! Durable run-history repository.
//!
//! Apply-class runs keep an immutable detail directory and an append-only JSONL index; Compare
//! keeps an index record only. Persisted vocabulary, path authority, strict codecs, migrations,
//! recording, queries, and retention are separate because corruption or path ambiguity must fail
//! before any cleanup or presentation action can proceed.

mod codec;
mod migration;
mod model;
mod paths;
mod recording;
mod relocation;
mod repository;
mod retention;
mod storage;

pub use model::{
    LatestRunRecord, LogArtifactKind, RunArtifacts, RunJobBinding, RunKind, RunRecord, RunSubject,
};
pub use paths::logs_dir;
pub use recording::{compare_summary, Recorder};
pub use relocation::{migrate_log_dir, MigrateReport};
pub use repository::{
    artifact_lines, history, history_merged, history_merged_for_registered_job, latest_by_job,
    with_validated_reveal_target,
};
pub use retention::prune;

#[cfg(test)]
use codec::{legacy_record_id, parse_current_record};
#[cfg(test)]
use migration::{
    ensure_current_schema_locked, migrate_current_schema_locked, migrate_legacy_record,
};
#[cfg(test)]
use model::{LegacyRunRecord, RUN_RECORD_SCHEMA};
#[cfg(test)]
use paths::{run_identifier, sanitize};
#[cfg(test)]
use recording::{create_run_dir, pending_record};
#[cfg(test)]
use repository::{
    artifact_lines_at, history_at, history_merged_at, with_validated_reveal_target_at,
};
#[cfg(test)]
use retention::{prune_at, sweep_orphans};

#[cfg(test)]
mod tests;
