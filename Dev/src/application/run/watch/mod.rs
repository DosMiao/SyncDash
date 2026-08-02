//! Pure orchestration for an optional filesystem-watcher trigger.
//!
//! This module does not open an OS watcher and does not run a compare.  An adapter first subscribes
//! every root, calls [`WatchTrigger::arm`] with the resulting position, and only then asks for the
//! bootstrap ticket.  That ordering closes the otherwise unavoidable gap between "initial scan"
//! and "watch started".
//!
//! Changed paths are hints about *when* another compare is needed.  [`WorkCoverage::IncrementalEligible`]
//! does not make the current scanner incremental; an integration must promote it to a full scan
//! until it has a separately verified partial-snapshot implementation.  Standard and Paranoid are
//! never eligible: their evidence contract requires a fresh full-tree verification on every run.

mod trigger;
mod validation;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use crate::fs::watch::{TriggerBatch, WatchInvalidation, WatchPosition};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RigorPolicy {
    Quick,
    Fast,
    Balanced,
    Standard,
    Paranoid,
}

impl RigorPolicy {
    pub fn requires_full_verification(self) -> bool {
        matches!(self, Self::Standard | Self::Paranoid)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Invalidation {
    /// Exact backend invalidations, retained together because one native batch can report several.
    WatchBackend(Vec<WatchInvalidation>),
    /// A stream journal/epoch changed or its native event ID moved backwards.
    CursorDiscontinuity,
    /// A path did not identify one of the armed streams or was not usable as a relative path.
    IncompletePathData,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FullScanReason {
    Bootstrap,
    /// A backend-owned maximum verification interval elapsed without a native change event.
    /// Native events are only trigger hints, so periodically rebuilding the full snapshot keeps
    /// the automation honest even when a platform journal is unavailable or incomplete.
    Periodic,
    EvidencePolicy(RigorPolicy),
    WatchInvalidated(Invalidation),
    ChangeSetTooLarge {
        limit: usize,
    },
}

/// The strongest coverage the watcher state permits.
///
/// The current SyncDash scanner accepts only whole-root scans, so callers must presently promote
/// `IncrementalEligible` to `FullTree`.  The distinction is retained here to make later partial
/// scan work prove its own safety without weakening Standard or Paranoid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkCoverage {
    FullTree { reason: FullScanReason },
    IncrementalEligible { changed_paths: Vec<ChangedPath> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkTicket {
    pub id: u64,
    /// The exact multi-root watcher position captured immediately before work starts.
    pub through: WatchPosition,
    pub coverage: WorkCoverage,
}

/// One changed relative path, qualified by the watcher stream that produced it.
///
/// Source and target can report the same relative spelling independently; keeping the stream in
/// the key prevents path aggregation from erasing which tree changed.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ChangedPath {
    pub stream: String,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeBatch {
    pub through: WatchPosition,
    pub changed_paths: Vec<ChangedPath>,
    pub invalidation: Option<Invalidation>,
}

impl From<TriggerBatch> for ChangeBatch {
    fn from(batch: TriggerBatch) -> Self {
        let TriggerBatch {
            position,
            changed_paths,
            invalidations,
        } = batch;
        Self {
            through: position,
            changed_paths: changed_paths
                .into_iter()
                .map(|path| ChangedPath {
                    stream: path.stream,
                    path: path.path,
                })
                .collect(),
            invalidation: (!invalidations.is_empty())
                .then_some(Invalidation::WatchBackend(invalidations)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchConfig {
    pub debounce_ms: u64,
    /// Above this many distinct path hints, retain constant memory and require a full scan.
    pub max_paths: usize,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 2_000,
            max_paths: 512,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WatchStateError {
    NotArmed,
    AlreadyArmed,
    InvalidPosition(String),
    TicketMismatch { active: Option<u64>, received: u64 },
}

impl std::fmt::Display for WatchStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotArmed => {
                f.write_str("watch streams must be armed before bootstrap work can start")
            }
            Self::AlreadyArmed => f.write_str("watch streams are already armed"),
            Self::InvalidPosition(reason) => write!(f, "invalid watcher position: {reason}"),
            Self::TicketMismatch { active, received } => {
                write!(
                    f,
                    "watch work ticket {received} does not match active ticket {active:?}"
                )
            }
        }
    }
}

impl std::error::Error for WatchStateError {}

/// A deterministic watcher trigger.  All time values are caller-provided monotonic milliseconds,
/// which keeps the state machine independent of threads, runtimes, and wall-clock jumps.
pub struct WatchTrigger {
    policy: RigorPolicy,
    config: WatchConfig,
    armed: bool,
    bootstrap_pending: bool,
    observed: Option<WatchPosition>,
    committed: Option<WatchPosition>,
    pending_paths: BTreeSet<ChangedPath>,
    full_scan: Option<FullScanReason>,
    debounce_until_ms: Option<u64>,
    active: Option<WorkTicket>,
    next_ticket_id: u64,
}
