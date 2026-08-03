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
mod tests;
