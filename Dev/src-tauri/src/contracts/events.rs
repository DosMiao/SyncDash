//! The run-event envelope delivered to a window during a Compare or Apply.
//!
//! `purpose` is an enum rather than a string because it selects which window is allowed to see
//! the event, and a typo in a string would silently deliver a Compare event to the progress
//! window.

use serde::{Deserialize, Serialize};
use syncdash::model::event::ProgressEvent;

/// Which run an event belongs to, and therefore which window may receive it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunEventPurposeDto {
    Compare,
    Apply,
}

/// One sequenced progress event, flattened over its `ProgressEvent` payload.
///
/// `sequence` is what lets a window that reconnects mid-run merge a replay with the events it
/// already has without duplicating or dropping any.
#[derive(Serialize, Clone, Debug, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct RunEventDto {
    #[ts(type = "number")]
    pub(crate) sequence: u64,
    #[ts(type = "number")]
    pub(crate) run_id: u64,
    pub(crate) purpose: RunEventPurposeDto,
    #[serde(flatten)]
    #[ts(flatten)]
    pub(crate) ev: ProgressEvent,
}
