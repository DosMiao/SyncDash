//! Compare evidence, presentation, and export wire contracts.

use serde::{Deserialize, Serialize};
use syncdash::model::plan::{Op, PlanHeader};
use syncdash::pipeline::compare;

/// Immutable identity for one successful Compare result.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct CompareOwner {
    pub(crate) identity: CompareIdentity,
    pub(crate) job_name: String,
}

/// The exact job-target revision used to look up retained results and execution status.
#[derive(Serialize, Clone, Debug, PartialEq, Eq, Hash, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct CompareScopeDto {
    pub(crate) job_id: String,
    #[ts(type = "number")]
    pub(crate) target_index: usize,
    pub(crate) config_revision: String,
}

/// One monotonic verification epoch and the Compare run currently associated with it, if launched.
#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct CompareVerificationAttemptDto {
    #[ts(type = "number")]
    pub(crate) verification_epoch: u64,
    #[ts(type = "number | null")]
    pub(crate) compare_run_id: Option<u64>,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct CompareWorkspaceSnapshotDto {
    pub(crate) plan: PlanDto,
    pub(crate) execution_status: CompareScopeExecutionStatusDto,
}

/// An exact retained-workspace lookup never encodes absence as null. Missing evidence and its
/// authoritative scope status are read under the same repository lock.
#[derive(Serialize, Clone, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum CompareResultForgetDto {
    Forgotten { cleanup_warning: Option<String> },
    AlreadyForgotten,
}

/// Identifies the backend AutoScan trigger for which Compare is being reviewed. The backend turns
/// this public cursor into a private, one-use permit; it is not itself execution authority.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct AutoScanCompareRequestDto {
    #[ts(type = "number")]
    pub(crate) generation: u64,
    #[ts(type = "number")]
    pub(crate) ticket_id: u64,
}

#[derive(Serialize, Deserialize, Clone, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
    /// The mtime equality window this comparison actually applied, in milliseconds.
    ///
    /// `pipeline::compare::MTIME_SLACK_MS` is only a floor. `run::local::compare` widens it to the
    /// coarser of the two backends' declared mtime precision before comparing, so an FTP LIST root
    /// — whole minutes — is judged on a far wider window than a local volume. Publishing the number
    /// the run used is what keeps a window cue from calling one side newer on a pair the engine
    /// already ruled the same instant. Projected from the retained `CompareOptions`, which owns it.
    #[ts(type = "number")]
    pub(crate) mtime_window_ms: i64,
    pub(crate) owner: CompareOwner,
}

/// One reviewed plan row submitted for preflight/apply. The backend reconstructs the operation from
/// the exact retained plan; the frontend never gets to submit an independent write instruction.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct ReviewedRowDecisionDto {
    #[ts(type = "number")]
    pub(crate) index: usize,
    pub(crate) direction_reversed: bool,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct CsvRowPresentationDto {
    #[ts(type = "number")]
    pub(crate) index: usize,
    pub(crate) included: bool,
    pub(crate) direction_reversed: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum CompareFileSideDto {
    Source,
    Target,
}

/// A page of identical rows from the bounded compare-result repository. Every request is
/// authenticated against the exact job, target, revision, and compare owner that produced it.
#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct IdenticalPage {
    #[ts(type = "number")]
    pub(crate) total: u64,
    pub(crate) rows: Vec<compare::evidence::IdenticalRow>,
}
