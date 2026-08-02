//! Persisted run-history vocabulary.

use serde::{Deserialize, Serialize};

use crate::foundation::names::{
    RUNLOG_ERRORS_FILE, RUNLOG_ITEMS_FILE, RUNLOG_PLAN_FILE as PLAN_FILE, RUNLOG_RUN_FILE,
    RUNLOG_SUMMARY_FILE as SUMMARY_FILE,
};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub enum LogArtifactKind {
    Run,
    Errors,
    Items,
    Plan,
    Summary,
}

impl LogArtifactKind {
    pub(super) fn file_name(self) -> &'static str {
        match self {
            Self::Run => RUNLOG_RUN_FILE,
            Self::Errors => RUNLOG_ERRORS_FILE,
            Self::Items => RUNLOG_ITEMS_FILE,
            Self::Plan => PLAN_FILE,
            Self::Summary => SUMMARY_FILE,
        }
    }
}

pub(super) const RUN_RECORD_SCHEMA: u32 = 2;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub enum RunKind {
    Apply,
    PeerApply,
    Compare,
    PeerCompare,
}

impl RunKind {
    pub(super) fn is_compare(self) -> bool {
        matches!(self, Self::Compare | Self::PeerCompare)
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::PeerApply => "peer-apply",
            Self::Compare => "compare",
            Self::PeerCompare => "peer-compare",
        }
    }
}

impl std::fmt::Display for RunKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub enum RunJobBinding {
    Registered { job_id: String },
    AdHoc,
    LegacyUnbound,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub struct RunSubject {
    pub job_name: String,
    pub binding: RunJobBinding,
    #[ts(type = "number | null")]
    pub target_index: Option<usize>,
}

impl RunSubject {
    pub fn for_job(job_name: &str, job: &crate::job::SingleTargetJob) -> Self {
        let job_id = &job.configuration().job_id;
        Self {
            job_name: job_name.to_owned(),
            binding: if job_id.is_empty() {
                RunJobBinding::AdHoc
            } else {
                RunJobBinding::Registered {
                    job_id: job_id.clone(),
                }
            },
            target_index: Some(job.target_index()),
        }
    }

    pub fn registered(job_name: &str, job_id: &str, target_index: usize) -> Self {
        Self {
            job_name: job_name.to_owned(),
            binding: RunJobBinding::Registered {
                job_id: job_id.to_owned(),
            },
            target_index: Some(target_index),
        }
    }

    pub(super) fn registered_job_id(&self) -> Option<&str> {
        match &self.binding {
            RunJobBinding::Registered { job_id } => Some(job_id),
            RunJobBinding::AdHoc | RunJobBinding::LegacyUnbound => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub enum RunArtifacts {
    Directory { run_id: String },
    LegacyFile { file_name: String },
    SummaryOnly,
    Unavailable,
}

impl RunArtifacts {
    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::Directory { run_id } => Some(run_id),
            Self::LegacyFile { .. } | Self::SummaryOnly | Self::Unavailable => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub struct RunRecord {
    #[ts(type = "number")]
    pub schema: u32,
    pub record_id: String,
    /// When the run started (unix ms)
    #[ts(type = "number")]
    pub ts_ms: i64,
    pub subject: RunSubject,
    pub kind: RunKind,
    #[ts(type = "number")]
    pub done: u64,
    #[ts(type = "number")]
    pub skipped: u64,
    #[ts(type = "number")]
    pub errors: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub elapsed_ms: u64,
    pub cancelled: bool,
    pub artifacts: RunArtifacts,
    /// How many warnings are in the error detail (the error count is in `errors`)
    #[ts(type = "number")]
    pub warnings: u64,
    /// compare-class: how many differences were found. None for apply-class
    #[ts(type = "number | null")]
    pub ops_found: Option<u64>,
    /// Whether the run went all the way through. `start` first writes a `finished:false` summary and
    /// `finish` overwrites it with true — a run killed midway has no index line (`finish` never ran),
    /// only a directory; this field is what lets that directory still say "I did not finish".
    pub finished: bool,
}

#[derive(Serialize, Clone, Debug, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub struct LatestRunRecord {
    pub job_id: String,
    pub record: RunRecord,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyRunRecord {
    pub(super) ts_ms: i64,
    pub(super) job: String,
    pub(super) kind: String,
    pub(super) done: u64,
    pub(super) skipped: u64,
    pub(super) errors: u64,
    pub(super) bytes: u64,
    pub(super) elapsed_ms: u64,
    pub(super) cancelled: bool,
    #[serde(default)]
    pub(super) run_id: Option<String>,
    #[serde(default)]
    pub(super) warnings: u64,
    #[serde(default)]
    pub(super) ops_found: Option<u64>,
    #[serde(default = "legacy_finished")]
    pub(super) finished: bool,
    #[serde(default)]
    pub(super) detail: Option<String>,
}

fn legacy_finished() -> bool {
    true
}
