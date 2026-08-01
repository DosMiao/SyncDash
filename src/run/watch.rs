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
    EvidencePolicy(RigorPolicy),
    WatchInvalidated(Invalidation),
    ChangeSetTooLarge { limit: usize },
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

impl WatchTrigger {
    pub fn new(policy: RigorPolicy, config: WatchConfig) -> Self {
        Self {
            policy,
            config,
            armed: false,
            bootstrap_pending: false,
            observed: None,
            committed: None,
            pending_paths: BTreeSet::new(),
            full_scan: None,
            debounce_until_ms: None,
            active: None,
            next_ticket_id: 1,
        }
    }

    /// Restore the last durably committed cursor before arming the live watcher.
    ///
    /// A valid checkpoint is not permission to skip bootstrap: SyncDash does not yet persist a
    /// complete snapshot that can be replayed from it.  It is retained for cursor continuity and
    /// for a future backend adapter that can replay journal events safely.
    pub fn restore_checkpoint(&mut self, position: WatchPosition) -> Result<(), WatchStateError> {
        if self.armed {
            return Err(WatchStateError::AlreadyArmed);
        }
        position
            .validate()
            .map_err(WatchStateError::InvalidPosition)?;
        self.committed = Some(position);
        Ok(())
    }

    /// Record that all underlying watch subscriptions are live.
    ///
    /// Calling `next_work` before this is an error, which makes watcher-before-bootstrap ordering
    /// executable rather than a comment in the eventual platform adapter.
    pub fn arm(&mut self, current: WatchPosition) -> Result<(), WatchStateError> {
        if self.armed {
            return Err(WatchStateError::AlreadyArmed);
        }
        current
            .validate()
            .map_err(WatchStateError::InvalidPosition)?;
        self.armed = true;
        self.bootstrap_pending = true;
        self.observed = Some(current);
        Ok(())
    }

    /// Latch a watcher batch.  Batches received while work is active remain pending and are
    /// debounced after that work finishes; they are never cleared by completion of the older
    /// ticket.
    pub fn observe(&mut self, batch: ChangeBatch, at_ms: u64) -> Result<(), WatchStateError> {
        if !self.armed {
            return Err(WatchStateError::NotArmed);
        }
        batch
            .through
            .validate()
            .map_err(WatchStateError::InvalidPosition)?;

        let ChangeBatch {
            through,
            changed_paths,
            invalidation,
        } = batch;
        let discontinuity = self
            .observed
            .as_ref()
            .is_some_and(|previous| !position_follows(previous, &through));
        let incomplete_paths = changed_paths.iter().any(|changed| {
            changed.stream.trim().is_empty()
                || !safe_relative_path(&changed.path)
                || !through.streams.contains_key(&changed.stream)
        });
        self.observed = Some(through);

        let invalidation = invalidation
            .or(discontinuity.then_some(Invalidation::CursorDiscontinuity))
            .or(incomplete_paths.then_some(Invalidation::IncompletePathData));
        let mut changed = false;
        if let Some(reason) = invalidation {
            self.pending_paths.clear();
            self.full_scan = Some(FullScanReason::WatchInvalidated(reason));
            changed = true;
        } else if self.full_scan.is_none() {
            for path in changed_paths {
                self.pending_paths.insert(path);
                changed = true;
                if self.pending_paths.len() > self.config.max_paths {
                    self.pending_paths.clear();
                    self.full_scan = Some(FullScanReason::ChangeSetTooLarge {
                        limit: self.config.max_paths,
                    });
                    changed = true;
                    break;
                }
            }
        } else if !changed_paths.is_empty() {
            // A full scan is already latched.  Do not retain an unbounded path set, but do extend
            // the quiet period for every later event while the worker is busy.
            changed = true;
        }

        if changed {
            self.debounce_until_ms = Some(at_ms.saturating_add(self.config.debounce_ms));
        }
        Ok(())
    }

    /// Return the next stable unit of work once the quiet period has elapsed.
    pub fn next_work(&mut self, now_ms: u64) -> Result<Option<WorkTicket>, WatchStateError> {
        if !self.armed {
            return Err(WatchStateError::NotArmed);
        }
        if self.active.is_some() {
            return Ok(None);
        }
        if !self.bootstrap_pending && self.full_scan.is_none() && self.pending_paths.is_empty() {
            return Ok(None);
        }
        if !self.bootstrap_pending
            && self
                .debounce_until_ms
                .is_some_and(|deadline| now_ms < deadline)
        {
            return Ok(None);
        }

        let through = self
            .observed
            .clone()
            .expect("an armed watcher always has an observed position");
        let coverage = if self.bootstrap_pending {
            WorkCoverage::FullTree {
                reason: FullScanReason::Bootstrap,
            }
        } else if let Some(reason) = self.full_scan.clone() {
            WorkCoverage::FullTree { reason }
        } else if self.policy.requires_full_verification() {
            WorkCoverage::FullTree {
                reason: FullScanReason::EvidencePolicy(self.policy),
            }
        } else {
            WorkCoverage::IncrementalEligible {
                changed_paths: self.pending_paths.iter().cloned().collect(),
            }
        };

        let ticket = WorkTicket {
            id: self.next_ticket_id,
            through,
            coverage,
        };
        self.next_ticket_id = self.next_ticket_id.wrapping_add(1).max(1);
        self.bootstrap_pending = false;
        self.pending_paths.clear();
        self.full_scan = None;
        self.debounce_until_ms = None;
        self.active = Some(ticket.clone());
        Ok(Some(ticket))
    }

    /// Publish a successful ticket's cursor, then advance the in-memory committed cursor.
    ///
    /// The closure is intentionally inside this transition.  If persistence fails, the ticket is
    /// re-latched and `committed_position` is unchanged, so a retry or process restart cannot skip
    /// work that never became durable.
    pub fn complete_success(
        &mut self,
        ticket_id: u64,
        persist: impl FnOnce(&WatchPosition) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let ticket = self
            .active_ticket(ticket_id)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        if let Err(error) = persist(&ticket.through) {
            self.active = None;
            self.requeue(ticket);
            return Err(error);
        }
        self.active = None;
        self.committed = Some(ticket.through);
        Ok(())
    }

    /// Re-latch failed work without advancing or persisting its cursor.
    pub fn complete_failure(&mut self, ticket_id: u64) -> Result<(), WatchStateError> {
        let ticket = self.active_ticket(ticket_id)?;
        self.active = None;
        self.requeue(ticket);
        Ok(())
    }

    pub fn committed_position(&self) -> Option<&WatchPosition> {
        self.committed.as_ref()
    }

    pub fn has_latched_work(&self) -> bool {
        self.bootstrap_pending || self.full_scan.is_some() || !self.pending_paths.is_empty()
    }

    fn active_ticket(&self, ticket_id: u64) -> Result<WorkTicket, WatchStateError> {
        match &self.active {
            Some(ticket) if ticket.id == ticket_id => Ok(ticket.clone()),
            active => Err(WatchStateError::TicketMismatch {
                active: active.as_ref().map(|ticket| ticket.id),
                received: ticket_id,
            }),
        }
    }

    fn requeue(&mut self, ticket: WorkTicket) {
        match ticket.coverage {
            WorkCoverage::FullTree { reason } => {
                if self.full_scan.is_none() {
                    self.full_scan = Some(reason);
                }
            }
            WorkCoverage::IncrementalEligible { changed_paths } => {
                if self.full_scan.is_none() {
                    self.pending_paths.extend(changed_paths);
                }
            }
        }
        // A new event observed during the failed work already owns a later debounce deadline.
        // With no newer event, retry immediately.
        if self.debounce_until_ms.is_none() {
            self.debounce_until_ms = Some(0);
        }
    }
}

fn position_follows(previous: &WatchPosition, next: &WatchPosition) -> bool {
    previous.streams.len() == next.streams.len()
        && previous.streams.iter().all(|(name, old)| {
            next.streams.get(name).is_some_and(|new| {
                new.journal_uuid == old.journal_uuid
                    && new.epoch == old.epoch
                    && new.event_id >= old.event_id
            })
        })
}

fn safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains('\\')
        && path.as_bytes().get(1) != Some(&b':')
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::fs::watch::{
        InvalidationKind, RootCursor, TriggerPath, WatchInvalidation, SOURCE_STREAM,
    };

    fn position(epoch: &str, event_id: u64) -> WatchPosition {
        WatchPosition {
            streams: BTreeMap::from([
                (
                    "source".into(),
                    RootCursor {
                        journal_uuid: Some(format!("source-{epoch}")),
                        epoch: format!("source-{epoch}"),
                        event_id,
                    },
                ),
                (
                    "target".into(),
                    RootCursor {
                        journal_uuid: Some(format!("target-{epoch}")),
                        epoch: format!("target-{epoch}"),
                        event_id: event_id + 100,
                    },
                ),
            ]),
        }
    }

    fn config(debounce_ms: u64, max_paths: usize) -> WatchConfig {
        WatchConfig {
            debounce_ms,
            max_paths,
        }
    }

    fn changed(path: &str) -> ChangedPath {
        changed_on("source", path)
    }

    fn changed_on(stream: &str, path: &str) -> ChangedPath {
        ChangedPath {
            stream: stream.into(),
            path: path.into(),
        }
    }

    fn finish_bootstrap(trigger: &mut WatchTrigger, current: WatchPosition) {
        trigger.arm(current.clone()).unwrap();
        let ticket = trigger.next_work(0).unwrap().unwrap();
        assert_eq!(
            ticket.coverage,
            WorkCoverage::FullTree {
                reason: FullScanReason::Bootstrap
            }
        );
        trigger.complete_success(ticket.id, |_| Ok(())).unwrap();
        assert_eq!(trigger.committed_position(), Some(&current));
    }

    #[test]
    fn watcher_must_be_armed_before_bootstrap_can_start() {
        let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(100, 512));
        assert_eq!(trigger.next_work(0), Err(WatchStateError::NotArmed));

        trigger.arm(position("a", 0)).unwrap();
        let ticket = trigger.next_work(0).unwrap().unwrap();
        assert_eq!(
            ticket.coverage,
            WorkCoverage::FullTree {
                reason: FullScanReason::Bootstrap
            }
        );
    }

    #[test]
    fn events_during_busy_work_are_latched_and_debounced() {
        let initial = position("a", 0);
        let changed_position = position("a", 1);
        let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(100, 512));
        trigger.arm(initial.clone()).unwrap();
        let bootstrap = trigger.next_work(0).unwrap().unwrap();

        trigger
            .observe(
                ChangeBatch {
                    through: changed_position.clone(),
                    changed_paths: vec![changed("src/a.txt")],
                    invalidation: None,
                },
                10,
            )
            .unwrap();
        assert!(
            trigger.next_work(10).unwrap().is_none(),
            "one ticket must remain active at a time"
        );
        trigger.complete_success(bootstrap.id, |_| Ok(())).unwrap();
        assert_eq!(trigger.committed_position(), Some(&initial));
        assert!(trigger.has_latched_work());
        assert!(trigger.next_work(109).unwrap().is_none());

        let follow_up = trigger.next_work(110).unwrap().unwrap();
        assert_eq!(follow_up.through, changed_position);
        assert_eq!(
            follow_up.coverage,
            WorkCoverage::IncrementalEligible {
                changed_paths: vec![changed("src/a.txt")]
            }
        );
    }

    #[test]
    fn every_new_batch_extends_the_quiet_period_and_paths_are_deduplicated() {
        let mut trigger = WatchTrigger::new(RigorPolicy::Fast, config(100, 512));
        finish_bootstrap(&mut trigger, position("a", 0));
        trigger
            .observe(
                ChangeBatch {
                    through: position("a", 1),
                    changed_paths: vec![changed("b"), changed("a")],
                    invalidation: None,
                },
                10,
            )
            .unwrap();
        trigger
            .observe(
                ChangeBatch {
                    through: position("a", 2),
                    changed_paths: vec![changed("a"), changed("c")],
                    invalidation: None,
                },
                90,
            )
            .unwrap();
        assert!(trigger.next_work(189).unwrap().is_none());
        let ticket = trigger.next_work(190).unwrap().unwrap();
        assert_eq!(
            ticket.coverage,
            WorkCoverage::IncrementalEligible {
                changed_paths: vec![changed("a"), changed("b"), changed("c")]
            }
        );
    }

    #[test]
    fn equal_relative_paths_on_source_and_target_remain_distinct() {
        let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(0, 512));
        finish_bootstrap(&mut trigger, position("a", 0));
        trigger
            .observe(
                ChangeBatch {
                    through: position("a", 1),
                    changed_paths: vec![changed_on("source", "same"), changed_on("target", "same")],
                    invalidation: None,
                },
                1,
            )
            .unwrap();
        assert_eq!(
            trigger.next_work(1).unwrap().unwrap().coverage,
            WorkCoverage::IncrementalEligible {
                changed_paths: vec![changed_on("source", "same"), changed_on("target", "same")]
            }
        );
    }

    #[test]
    fn native_batches_preserve_cursors_paths_and_every_invalidation() {
        let through = position("a", 7);
        let native_invalidations = vec![
            WatchInvalidation {
                stream: SOURCE_STREAM.into(),
                kind: InvalidationKind::UserDropped,
            },
            WatchInvalidation {
                stream: SOURCE_STREAM.into(),
                kind: InvalidationKind::WholeRootChanged,
            },
        ];
        let batch = ChangeBatch::from(TriggerBatch {
            position: through.clone(),
            changed_paths: vec![TriggerPath {
                stream: SOURCE_STREAM.into(),
                path: "src/lib.rs".into(),
            }],
            invalidations: native_invalidations.clone(),
        });

        assert_eq!(batch.through, through);
        assert_eq!(batch.changed_paths, vec![changed("src/lib.rs")]);
        let expected_invalidation = Invalidation::WatchBackend(native_invalidations);
        assert_eq!(batch.invalidation, Some(expected_invalidation.clone()));

        let mut trigger = WatchTrigger::new(RigorPolicy::Balanced, config(0, 512));
        finish_bootstrap(&mut trigger, position("a", 0));
        trigger.observe(batch, 1).unwrap();
        assert_eq!(
            trigger.next_work(1).unwrap().unwrap().coverage,
            WorkCoverage::FullTree {
                reason: FullScanReason::WatchInvalidated(expected_invalidation)
            }
        );
    }

    #[test]
    fn overflow_and_large_batches_degrade_to_full_tree_work() {
        let mut overflow = WatchTrigger::new(RigorPolicy::Quick, config(0, 512));
        finish_bootstrap(&mut overflow, position("a", 0));
        let dropped = Invalidation::WatchBackend(vec![WatchInvalidation {
            stream: SOURCE_STREAM.into(),
            kind: InvalidationKind::UserDropped,
        }]);
        overflow
            .observe(
                ChangeBatch {
                    through: position("a", 1),
                    changed_paths: Vec::new(),
                    invalidation: Some(dropped.clone()),
                },
                1,
            )
            .unwrap();
        assert_eq!(
            overflow.next_work(1).unwrap().unwrap().coverage,
            WorkCoverage::FullTree {
                reason: FullScanReason::WatchInvalidated(dropped)
            }
        );

        let mut large = WatchTrigger::new(RigorPolicy::Fast, config(0, 2));
        finish_bootstrap(&mut large, position("a", 0));
        large
            .observe(
                ChangeBatch {
                    through: position("a", 1),
                    changed_paths: vec![changed("a"), changed("b"), changed("c")],
                    invalidation: None,
                },
                1,
            )
            .unwrap();
        assert_eq!(
            large.next_work(1).unwrap().unwrap().coverage,
            WorkCoverage::FullTree {
                reason: FullScanReason::ChangeSetTooLarge { limit: 2 }
            }
        );
    }

    #[test]
    fn cursor_reset_forces_a_full_scan_instead_of_skipping_events() {
        let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(0, 512));
        finish_bootstrap(&mut trigger, position("old", 40));
        trigger
            .observe(
                ChangeBatch {
                    through: position("new", 1),
                    changed_paths: vec![changed("a")],
                    invalidation: None,
                },
                1,
            )
            .unwrap();
        assert_eq!(
            trigger.next_work(1).unwrap().unwrap().coverage,
            WorkCoverage::FullTree {
                reason: FullScanReason::WatchInvalidated(Invalidation::CursorDiscontinuity)
            }
        );
    }

    #[test]
    fn changed_journal_uuid_is_a_discontinuity_even_if_the_epoch_was_reused() {
        let initial = position("same-epoch", 40);
        let mut changed_position = position("same-epoch", 41);
        changed_position
            .streams
            .get_mut(SOURCE_STREAM)
            .unwrap()
            .journal_uuid = Some("replacement-journal".into());
        let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(0, 512));
        finish_bootstrap(&mut trigger, initial);
        trigger
            .observe(
                ChangeBatch {
                    through: changed_position,
                    changed_paths: vec![changed("a")],
                    invalidation: None,
                },
                1,
            )
            .unwrap();
        assert_eq!(
            trigger.next_work(1).unwrap().unwrap().coverage,
            WorkCoverage::FullTree {
                reason: FullScanReason::WatchInvalidated(Invalidation::CursorDiscontinuity)
            }
        );
    }

    #[test]
    fn a_path_for_an_unknown_stream_forces_a_full_scan() {
        let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(0, 512));
        finish_bootstrap(&mut trigger, position("a", 0));
        trigger
            .observe(
                ChangeBatch {
                    through: position("a", 1),
                    changed_paths: vec![changed_on("not-armed", "a")],
                    invalidation: None,
                },
                1,
            )
            .unwrap();
        assert_eq!(
            trigger.next_work(1).unwrap().unwrap().coverage,
            WorkCoverage::FullTree {
                reason: FullScanReason::WatchInvalidated(Invalidation::IncompletePathData)
            }
        );
    }

    #[test]
    fn an_unsafe_relative_path_forces_a_full_scan() {
        let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(0, 512));
        finish_bootstrap(&mut trigger, position("a", 0));
        trigger
            .observe(
                ChangeBatch {
                    through: position("a", 1),
                    changed_paths: vec![changed("../outside")],
                    invalidation: None,
                },
                1,
            )
            .unwrap();
        assert_eq!(
            trigger.next_work(1).unwrap().unwrap().coverage,
            WorkCoverage::FullTree {
                reason: FullScanReason::WatchInvalidated(Invalidation::IncompletePathData)
            }
        );
    }

    #[test]
    fn standard_and_paranoid_always_require_full_verification() {
        for policy in [RigorPolicy::Standard, RigorPolicy::Paranoid] {
            let mut trigger = WatchTrigger::new(policy, config(0, 512));
            finish_bootstrap(&mut trigger, position("a", 0));
            trigger
                .observe(
                    ChangeBatch {
                        through: position("a", 1),
                        changed_paths: vec![changed("one-small-file")],
                        invalidation: None,
                    },
                    1,
                )
                .unwrap();
            let ticket = trigger.next_work(1).unwrap().unwrap();
            assert_eq!(
                ticket.coverage,
                WorkCoverage::FullTree {
                    reason: FullScanReason::EvidencePolicy(policy)
                }
            );
        }
    }

    #[test]
    fn balanced_remains_incremental_eligible_like_fast() {
        let mut trigger = WatchTrigger::new(RigorPolicy::Balanced, config(0, 512));
        finish_bootstrap(&mut trigger, position("a", 0));
        trigger
            .observe(
                ChangeBatch {
                    through: position("a", 1),
                    changed_paths: vec![changed("one-small-file")],
                    invalidation: None,
                },
                1,
            )
            .unwrap();
        assert_eq!(
            trigger.next_work(1).unwrap().unwrap().coverage,
            WorkCoverage::IncrementalEligible {
                changed_paths: vec![changed("one-small-file")]
            }
        );
    }

    #[test]
    fn cursor_advances_only_after_work_and_checkpoint_persistence_both_succeed() {
        let durable = position("a", 1);
        let current = position("a", 5);
        let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(0, 512));
        trigger.restore_checkpoint(durable.clone()).unwrap();
        trigger.arm(current.clone()).unwrap();
        let first = trigger.next_work(0).unwrap().unwrap();

        let error = trigger
            .complete_success(first.id, |_| Err(std::io::Error::other("disk full")))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(trigger.committed_position(), Some(&durable));

        let retry = trigger.next_work(0).unwrap().unwrap();
        assert_eq!(retry.through, current);
        let mut persisted = None;
        trigger
            .complete_success(retry.id, |position| {
                persisted = Some(position.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(persisted.as_ref(), Some(&current));
        assert_eq!(trigger.committed_position(), Some(&current));
    }

    #[test]
    fn failed_compare_is_retried_without_advancing_the_cursor() {
        let initial = position("a", 0);
        let changed_position = position("a", 1);
        let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(0, 512));
        finish_bootstrap(&mut trigger, initial.clone());
        trigger
            .observe(
                ChangeBatch {
                    through: changed_position.clone(),
                    changed_paths: vec![changed("a")],
                    invalidation: None,
                },
                1,
            )
            .unwrap();
        let failed = trigger.next_work(1).unwrap().unwrap();
        trigger.complete_failure(failed.id).unwrap();
        assert_eq!(trigger.committed_position(), Some(&initial));

        let retry = trigger.next_work(1).unwrap().unwrap();
        assert_eq!(retry.through, changed_position);
        assert_eq!(retry.coverage, failed.coverage);
    }
}
