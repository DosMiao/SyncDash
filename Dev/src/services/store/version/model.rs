//! The persisted vocabulary of a version store: what a preserved entry is, and how it is indexed.
//!
//! These shapes are written to disk and read back by a later build, so they are a format rather
//! than an implementation detail. Kept apart from the writer that produces them and the restore
//! that consumes them, because a reader must be able to understand an index without linking either.

use serde::{Deserialize, Serialize};

use crate::foundation::path::{EntryName, RootRelativePath};
use crate::model::chunk::RecipeStep;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionPayloadKind {
    #[serde(rename = "whole")]
    Whole,
    #[serde(rename = "rdelta")]
    ReverseDelta,
}

impl std::fmt::Display for VersionPayloadKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Whole => "whole",
            Self::ReverseDelta => "rdelta",
        })
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PreservedEntry {
    #[serde(rename = "rel")]
    pub relative_path: RootRelativePath,
    #[serde(rename = "kind")]
    pub payload_kind: VersionPayloadKind,
    #[serde(rename = "why")]
    pub reason: String,
    pub old_hash: String,
    pub old_size: u64,
    pub old_mtime_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub old_mode: Option<u32>,
    /// rdelta: the hash the current file must match at reassembly time
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub new_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub recipe: Option<Vec<RecipeStep>>,
}

#[derive(Serialize, Deserialize)]
pub struct VersionManifest {
    pub id: EntryName,
    pub ts_ms: u64,
    pub host: String,
    pub entries: Vec<PreservedEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct VersionIndexEntry {
    pub id: EntryName,
    pub ts_ms: u64,
    pub host: String,
    pub ops: u64,
    pub preserved: u64,
    pub bytes: u64,
}
