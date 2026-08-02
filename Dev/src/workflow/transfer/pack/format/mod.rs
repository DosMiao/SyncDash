//! Versioned package wire model and validation-aware tar decoding.

pub(in crate::transfer::pack) mod codec;
mod validation;

use serde::{Deserialize, Serialize};

use crate::foundation::path::RootRelativePath;
use crate::model::chunk::RecipeStep;

/// Version of the complete package protocol accepted by this binary.
pub const PACK_VERSION: u32 = 2;

/// Schema of `manifest.json` within the current package protocol.
pub(super) const PACK_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize, Clone)]
pub struct PayloadEntry {
    pub rel: RootRelativePath,
    pub size: u64,
    pub mtime_ms: i64,
    /// BLAKE3 of the final whole file, checked after reassembly and extraction.
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<u32>,
    /// "whole" (default) | "delta".
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Delta: the BLAKE3 the target's existing file must match.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_hash: Option<String>,
    /// Delta: BLAKE3 of the tar's `payload/<rel>` entry.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub blob_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub recipe: Option<Vec<RecipeStep>>,
}

fn default_kind() -> String {
    "whole".into()
}

#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    #[serde(default)]
    pub pack_version: u32,
    pub created_ms: u64,
    pub source_host: String,
    pub source_os: String,
    pub target_root_hint: String,
    pub op_count: u64,
    pub plan_blake3: String,
    pub payload: Vec<PayloadEntry>,
    pub payload_combined_blake3: String,
}

pub struct PackSummary {
    pub ops: u64,
    pub files: u64,
    pub bytes: u64,
    /// Bytes saved by delta transfer (whole-file size minus the blob actually packed).
    pub delta_saved: u64,
}

pub(super) struct DecodedPackage {
    pub(super) plan: crate::model::plan::Plan,
    pub(super) manifest: Manifest,
}

pub(super) fn package_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}
