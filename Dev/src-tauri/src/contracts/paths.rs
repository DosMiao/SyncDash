//! Endpoint-inspection wire contracts.

use serde::Serialize;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
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
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct PathVerdict {
    pub(crate) source: PathInfo,
    pub(crate) target: PathInfo,
    /// Plain-language warnings; the editor renders them right under the field
    pub(crate) warnings: Vec<String>,
    /// Readiness facts that are informative rather than failures, such as a network probe deferred
    /// until Compare owns credentials and a cancellation context.
    pub(crate) notes: Vec<String>,
}
