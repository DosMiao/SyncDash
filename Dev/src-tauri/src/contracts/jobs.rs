//! Job-registry and editor wire contracts.

use serde::Serialize;

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct JobDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) mode: String,
    pub(crate) rigor: String,
    pub(crate) source: String,
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
    /// The share of a side's entries past which the review panel colors a category. It is the same
    /// number the job's Gates chip shows, so what the operator configured is what turns red.
    pub(crate) max_delete_ratio: f64,
    /// The canonical target roots; a single-target job has exactly one entry.
    pub(crate) targets: Vec<String>,
    pub(crate) config_revision: String,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct JobDetailDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) job: syncdash::job::Job,
    /// Present exactly when the transport router routes this job to a peer. Derived here from the
    /// same target phrases the job carries, so no reader downstream re-parses a root phrase: a
    /// string test on the phrase disagrees with `spec::parse` about an upper-case scheme, a spaced
    /// `| mount = /x`, and an empty `|mount=`.
    pub(crate) peer_link: Option<PeerLinkDto>,
    pub(crate) config_revision: String,
}

/// What a peer target declares beyond its own phrase.
#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct PeerLinkDto {
    /// The local path source-side (pull) operations write through, or `null` for a push-only job.
    /// Null covers both an absent and an empty `|mount=`, because Apply refuses the pull for either.
    pub(crate) pull_mount: Option<String>,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct JobSaveDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) config_revision: String,
    pub(crate) effect: syncdash::job::JobMutationEffect,
    pub(crate) status_delivery_warnings: Vec<String>,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct JobRootMutationDto {
    pub(crate) mutation: JobSaveDto,
    pub(crate) source: String,
    pub(crate) targets: Vec<String>,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct JobDeleteDto {
    pub(crate) job_id: String,
    pub(crate) name: String,
    pub(crate) config_revision: String,
    pub(crate) effect: syncdash::job::JobMutationEffect,
    pub(crate) status_delivery_warnings: Vec<String>,
}

/// The `schema` recorded in the job file **as it sits on disk**, next to the version this build writes.
/// `get_job` returns the migrated job, so by then the difference is gone — but the editor is showing
/// exclude lines the file does not contain yet, and it has to be able to say so.
/// The two differing means the v1 junk keys were expanded into `exclude` on load.
#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct JobFileSchemaDto {
    #[ts(type = "number")]
    pub(crate) on_disk: u32,
    #[ts(type = "number")]
    pub(crate) current: u32,
}

#[derive(Serialize, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct JunkPresetDto {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) hint: String,
    pub(crate) patterns: Vec<String>,
}
