//! The wire types. Every one is exported by ts-rs into `typescript/core/types/generated/`, so the
//! frontend never hand-writes a shape the Rust side owns.

use serde::{Deserialize, Serialize};

use syncdash::model::plan::{Op, PlanHeader};
use syncdash::pipeline::compare;

#[derive(Serialize, Clone, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct SettingsNumericLimitsDto {
    #[ts(type = "number")]
    pub(crate) maximum_keep_days: u64,
    #[ts(type = "number")]
    pub(crate) maximum_total_mb: u64,
}

#[derive(Serialize, Clone, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct SettingsSnapshotDto {
    pub(crate) settings: syncdash::store::settings::AppSettings,
    pub(crate) revision: String,
    pub(crate) diagnostic: Option<String>,
    pub(crate) numeric_limits: SettingsNumericLimitsDto,
}

impl From<syncdash::store::settings::AppSettingsSnapshot> for SettingsSnapshotDto {
    fn from(snapshot: syncdash::store::settings::AppSettingsSnapshot) -> Self {
        Self {
            settings: snapshot.settings,
            revision: snapshot.revision,
            diagnostic: snapshot.diagnostic,
            numeric_limits: SettingsNumericLimitsDto {
                maximum_keep_days: syncdash::store::settings::MAX_KEEP_DAYS,
                maximum_total_mb: syncdash::store::settings::MAX_TOTAL_MB,
            },
        }
    }
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct LogDirectorySelectionDto {
    pub(crate) directory: String,
    pub(crate) grant_id: Option<String>,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct SettingsSaveDto {
    pub(crate) snapshot: SettingsSnapshotDto,
    pub(crate) migration: syncdash::store::migrate::MigrateReport,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) mode: String,
    pub(crate) rigor: String,
    pub(crate) source: String,
    pub(crate) has_archive: bool,
    /// Derived from the root phrase; no separate host field may duplicate that authority.
    pub(crate) is_peer_job: bool,
    pub(crate) versioning: bool,
    pub(crate) delta: bool,
    #[ts(type = "number | null")]
    pub(crate) parallel: Option<usize>,
    pub(crate) include: Vec<String>,
    pub(crate) exclude: Vec<String>,
    #[ts(type = "number | null")]
    pub(crate) autoscan_interval_secs: Option<u64>,
    pub(crate) autoscan_auto_apply: bool,
    /// The canonical target roots; a single-target job has exactly one entry.
    pub(crate) targets: Vec<String>,
    pub(crate) config_revision: String,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobDetailDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) job: syncdash::job::Job,
    pub(crate) config_revision: String,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobSaveDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) config_revision: String,
    pub(crate) effect: syncdash::job::JobMutationEffect,
    pub(crate) previous_name: Option<String>,
    pub(crate) status_delivery_warnings: Vec<String>,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobRootMutationDto {
    pub(crate) mutation: JobSaveDto,
    pub(crate) source: String,
    pub(crate) targets: Vec<String>,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobDeleteDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) config_revision: String,
    pub(crate) effect: syncdash::job::JobMutationEffect,
    pub(crate) status_delivery_warnings: Vec<String>,
}

/// Immutable identity for one successful Compare result.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct CompareIdentity {
    pub(crate) result_id: String,
    #[ts(type = "number")]
    pub(crate) compare_run_id: u64,
    pub(crate) job_id: String,
    #[ts(type = "number")]
    pub(crate) target_index: usize,
    pub(crate) config_revision: String,
}

/// An immutable Compare identity paired with its current display label.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct CompareOwner {
    pub(crate) identity: CompareIdentity,
    pub(crate) job_name: String,
}

/// The exact job-target revision used to look up retained results and execution status.
#[derive(Serialize, Clone, Debug, PartialEq, Eq, Hash, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct CompareScopeDto {
    pub(crate) job_id: String,
    #[ts(type = "number")]
    pub(crate) target_index: usize,
    pub(crate) config_revision: String,
}

/// One monotonic verification epoch and the Compare run currently associated with it, if launched.
#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct CompareVerificationAttemptDto {
    #[ts(type = "number")]
    pub(crate) verification_epoch: u64,
    #[ts(type = "number | null")]
    pub(crate) compare_run_id: Option<u64>,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum CompareExecutionExpiryReasonDto {
    ApplicationRestarted,
    JobChanged,
    JobDeleted,
    WriteStarted,
    VerificationExhausted,
}

/// Authoritative execution authority for one Compare scope. Retained plans remain viewable even
/// when this status says that their filesystem evidence is no longer eligible for Apply.
#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum CompareScopeExecutionStatusDto {
    Unavailable {
        scope: CompareScopeDto,
    },
    AwaitingCompare {
        scope: CompareScopeDto,
        attempt: CompareVerificationAttemptDto,
    },
    Comparing {
        scope: CompareScopeDto,
        attempt: CompareVerificationAttemptDto,
    },
    Fresh {
        scope: CompareScopeDto,
        attempt: CompareVerificationAttemptDto,
        owner: CompareOwner,
    },
    Failed {
        scope: CompareScopeDto,
        attempt: CompareVerificationAttemptDto,
        message: String,
    },
    Cancelled {
        scope: CompareScopeDto,
        attempt: CompareVerificationAttemptDto,
    },
    Expired {
        scope: CompareScopeDto,
        attempt: CompareVerificationAttemptDto,
        reason: CompareExecutionExpiryReasonDto,
    },
}

/// An atomic restored Compare workspace: immutable plan evidence and its independently evolving
/// execution status are read under the same repository lock.
#[derive(Serialize, Clone, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct CompareWorkspaceSnapshotDto {
    pub(crate) plan: PlanDto,
    pub(crate) execution_status: CompareScopeExecutionStatusDto,
}

/// An exact retained-workspace lookup never encodes absence as null. Missing evidence and its
/// authoritative scope status are read under the same repository lock.
#[derive(Serialize, Clone, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum CompareWorkspaceLookupDto {
    Found {
        workspace: Box<CompareWorkspaceSnapshotDto>,
    },
    Missing {
        execution_status: CompareScopeExecutionStatusDto,
    },
}

#[derive(Serialize, Clone, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum CompareResultForgetDto {
    Forgotten { cleanup_warning: Option<String> },
    AlreadyForgotten,
}

/// Identifies the backend AutoScan trigger for which Compare is being reviewed. The backend turns
/// this public cursor into a private, one-use permit; it is not itself execution authority.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct AutoScanCompareRequestDto {
    #[ts(type = "number")]
    pub(crate) generation: u64,
    #[ts(type = "number")]
    pub(crate) ticket_id: u64,
}

#[derive(Serialize, Deserialize, Clone, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct ReviewedRowDecisionDto {
    #[ts(type = "number")]
    pub(crate) index: usize,
    pub(crate) direction_reversed: bool,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct CsvRowPresentationDto {
    #[ts(type = "number")]
    pub(crate) index: usize,
    pub(crate) included: bool,
    pub(crate) direction_reversed: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum CsvExportDto {
    Cancelled,
    Exported {
        #[ts(type = "number")]
        row_count: usize,
        display_path: String,
        receipt_id: String,
    },
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum CompareFileSideDto {
    Source,
    Target,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum PostRunPowerActionDto {
    Sleep,
    Shutdown,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct PostRunPowerActionReadyDto {
    #[ts(type = "number")]
    pub(crate) run_id: u64,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct AuthorizationDto {
    pub(crate) authorization_token: String,
    #[ts(type = "number")]
    pub(crate) expires_at_ms: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum OperationReviewDto {
    Blocked {
        blockers: Vec<String>,
        warnings: Vec<String>,
        capabilities: Vec<CapabilityIssueDto>,
    },
    DirectAuthorized {
        authorization: AuthorizationDto,
        capabilities: Vec<CapabilityIssueDto>,
    },
    CompareConfirmationRequired {
        challenge_id: String,
        #[ts(type = "number")]
        expires_at_ms: u64,
        capabilities: Vec<CapabilityIssueDto>,
        can_remember_for_session: bool,
    },
    InteractiveApplyConfirmationRequired {
        challenge_id: String,
        #[ts(type = "number")]
        expires_at_ms: u64,
        warnings: Vec<String>,
        capabilities: Vec<CapabilityIssueDto>,
        requires_health_ack: bool,
        requires_capability_ack: bool,
        can_remember_for_session: bool,
        can_allow_unattended: bool,
    },
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "operation", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum OperationApprovalDto {
    Compare {
        accept_capabilities: bool,
        remember_for_session: bool,
    },
    InteractiveApply {
        acknowledge_health: bool,
        accept_capabilities: bool,
        session_grant: ApplySessionGrantDecisionDto,
    },
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) enum ApplySessionGrantDecisionDto {
    None,
    RememberCapabilities,
    AllowAutoApply,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
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
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct JobFileSchemaDto {
    #[ts(type = "number")]
    pub(crate) on_disk: u32,
    #[ts(type = "number")]
    pub(crate) current: u32,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../typescript/core/types/generated/")]
pub(crate) struct JunkPresetDto {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) hint: String,
    pub(crate) patterns: Vec<String>,
    pub(crate) default_on: bool,
}
