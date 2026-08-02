//! Public results and commands for compare-and-swap job registry mutations.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub enum JobMutationEffect {
    Created,
    Updated,
    Renamed,
    NoOp,
    Deleted,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub enum JobRootField {
    Source,
    Target,
}

#[derive(Debug)]
pub struct SavedJob {
    pub name: String,
    pub path: PathBuf,
    pub job_id: String,
    pub config_revision: String,
    pub effect: JobMutationEffect,
    pub previous_name: Option<String>,
}

#[derive(Debug)]
pub struct JobRootMutation {
    pub mutation: SavedJob,
    pub source: String,
    pub targets: Vec<String>,
}

#[derive(Debug)]
pub struct DeletedJob {
    pub name: String,
    pub job_id: String,
    pub config_revision: String,
    pub effect: JobMutationEffect,
}
