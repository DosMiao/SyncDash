//! The wire types. Every one is exported by ts-rs into `typescript/core/types/generated/`, so the
//! frontend never hand-writes a shape the Rust side owns.

use serde::{Deserialize, Serialize};

use syncdash::model::plan::{Op, PlanHeader};
use syncdash::pipeline::compare;

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) mode: String,
    pub(crate) rigor: String,
    pub(crate) source: String,
    pub(crate) target: String,
    pub(crate) has_archive: bool,
    // v0.9 M3: fill in the fields the frontend needs to see (remote badge / versioning marker / filter hints).
    // The host is not among them: it lives in `target` now (`peer://<host>/…`), and shipping a second
    // copy is how the frontend ends up rendering a stale one.
    pub(crate) remote: bool,
    pub(crate) versioning: bool,
    pub(crate) delta: bool,
    #[ts(type = "number | null")]
    pub(crate) parallel: Option<usize>,
    pub(crate) include: Vec<String>,
    pub(crate) exclude: Vec<String>,
    #[ts(type = "number | null")]
    pub(crate) watch_interval_secs: Option<u64>,
    pub(crate) watch_auto_apply: bool,
    /// 1:N: the effective target list (a single-target job = one entry). When >1 the frontend shows the target selector
    pub(crate) targets: Vec<String>,
    pub(crate) config_revision: String,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobDetailDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) job: syncdash::job::Job,
    pub(crate) config_revision: String,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobSaveDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) config_revision: String,
    pub(crate) effect: syncdash::job::JobMutationEffect,
    pub(crate) previous_name: Option<String>,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobDeleteDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) config_revision: String,
    pub(crate) effect: syncdash::job::JobMutationEffect,
}

/// Stable authority-bearing identity for one successful Compare run.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct CompareIdentity {
    #[ts(type = "number")]
    pub(crate) compare_run_id: u64,
    pub(crate) job_id: String,
    #[ts(type = "number")]
    pub(crate) target_index: usize,
    pub(crate) config_revision: String,
}

/// A stable Compare identity paired with its current presentation label.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct CompareOwner {
    pub(crate) identity: CompareIdentity,
    pub(crate) job_name: String,
}

#[derive(Serialize, Deserialize, Clone, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct PlanDto {
    pub(crate) header: PlanHeader,
    pub(crate) ops: Vec<Op>,
    /// One entry per op: the size/mtime measured on both sides at compare time (for the table columns and sorting).
    /// Copy rows already carry their sole side's size/mtime in `Op`, so their entry is null and the
    /// frontend reconstructs it. This matters at six figures: two nested JSON objects per copy row
    /// otherwise account for most of WebKit's retained allocations.
    #[serde(default)]
    pub(crate) metas: Vec<Option<compare::evidence::RowMeta>>,
    /// Count/bytes of files this comparison judged identical on both sides.
    #[serde(default)]
    #[ts(type = "number")]
    pub(crate) identical_count: u64,
    #[serde(default)]
    #[ts(type = "number")]
    pub(crate) identical_bytes: u64,
    pub(crate) owner: CompareOwner,
}

/// One reviewed plan row submitted for preflight/apply. The backend reconstructs the operation from
/// the exact retained plan; the frontend never gets to submit an independent write instruction.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct SelectedRowDto {
    #[ts(type = "number")]
    pub(crate) index: usize,
    pub(crate) flipped: bool,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) enum ReviewStatus {
    DirectAuthorized,
    ConfirmationRequired,
    Blocked,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) enum CapabilitySeverityDto {
    Block,
    NeedsAck,
    Info,
}

impl From<syncdash::pipeline::guard::caps::CapSeverity> for CapabilitySeverityDto {
    fn from(value: syncdash::pipeline::guard::caps::CapSeverity) -> Self {
        match value {
            syncdash::pipeline::guard::caps::CapSeverity::Block => Self::Block,
            syncdash::pipeline::guard::caps::CapSeverity::NeedsAck => Self::NeedsAck,
            syncdash::pipeline::guard::caps::CapSeverity::Info => Self::Info,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct CapabilityIssueDto {
    pub(crate) feature: String,
    pub(crate) side: String,
    pub(crate) severity: CapabilitySeverityDto,
    pub(crate) requested: String,
    pub(crate) actual: String,
    pub(crate) effect: String,
}

impl From<&syncdash::pipeline::guard::caps::CapItem> for CapabilityIssueDto {
    fn from(value: &syncdash::pipeline::guard::caps::CapItem) -> Self {
        Self {
            feature: value.feature.clone(),
            side: value.side.clone(),
            severity: value.severity.into(),
            requested: value.requested.clone(),
            actual: value.actual.clone(),
            effect: value.effect.clone(),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct AuthorizationDto {
    pub(crate) authorization_token: String,
    #[ts(type = "number")]
    pub(crate) expires_at_ms: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct OperationReviewDto {
    pub(crate) status: ReviewStatus,
    pub(crate) authorization: Option<AuthorizationDto>,
    pub(crate) challenge_id: Option<String>,
    #[ts(type = "number | null")]
    pub(crate) expires_at_ms: Option<u64>,
    pub(crate) blockers: Vec<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) capabilities: Vec<CapabilityIssueDto>,
    pub(crate) requires_health_ack: bool,
    pub(crate) requires_capability_ack: bool,
    pub(crate) can_remember_for_session: bool,
    pub(crate) can_allow_unattended: bool,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct ApplyDto {
    #[ts(type = "number")]
    pub(crate) done: u64,
    #[ts(type = "number")]
    pub(crate) skipped: u64,
    #[ts(type = "number")]
    pub(crate) errors: u64,
    #[ts(type = "number")]
    pub(crate) bytes_copied: u64,
    pub(crate) cancelled: bool,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) enum EndpointReadiness {
    Empty,
    Ready,
    Missing,
    NotDirectory,
    Deferred,
    Invalid,
    Unobservable,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct PathInfo {
    pub(crate) readiness: EndpointReadiness,
    pub(crate) exists: Option<bool>,
    pub(crate) is_dir: Option<bool>,
    pub(crate) has_marker: Option<bool>,
}

impl Default for PathInfo {
    fn default() -> Self {
        Self {
            readiness: EndpointReadiness::Empty,
            exists: None,
            is_dir: None,
            has_marker: None,
        }
    }
}

#[derive(Serialize, Default, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct PathVerdict {
    pub(crate) source: PathInfo,
    pub(crate) target: PathInfo,
    /// Plain-language warnings; the editor renders them right under the field
    pub(crate) warnings: Vec<String>,
    /// Readiness facts that are informative rather than failures, such as a network probe deferred
    /// until Compare owns credentials and a cancellation context.
    pub(crate) notes: Vec<String>,
}

/// A page of identical rows from the bounded compare-result repository. Every request is
/// authenticated against the exact job, target, revision, and compare owner that produced it.
#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct IdenticalPage {
    #[ts(type = "number")]
    pub(crate) total: u64,
    pub(crate) rows: Vec<compare::evidence::IdenticalRow>,
}

/// The `schema` recorded in the job file **as it sits on disk**, next to the version this build writes.
/// `get_job` returns the migrated job, so by then the difference is gone — but the editor is showing
/// exclude lines the file does not contain yet, and it has to be able to say so.
/// The two differing means the v1 junk keys were expanded into `exclude` on load.
#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobFileSchemaDto {
    #[ts(type = "number")]
    pub(crate) on_disk: u32,
    #[ts(type = "number")]
    pub(crate) current: u32,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub(crate) struct JunkPresetDto {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) hint: String,
    pub(crate) patterns: Vec<String>,
    pub(crate) default_on: bool,
}
