//! State transitions for watcher observations, work tickets, and durable completion.

use super::validation::{position_follows, safe_relative_path};
use super::*;

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

    /// Latch a full-tree verification at a known live watcher position.
    ///
    /// The desktop adapter uses this for its maximum verification interval. It supplies the
    /// watcher's position captured immediately before this call, so a successful compare can
    /// durably commit exactly the events it covered. A periodic request never erases a stronger
    /// invalidation or a change batch already waiting to run.
    pub fn request_periodic(
        &mut self,
        through: WatchPosition,
        at_ms: u64,
    ) -> Result<(), WatchStateError> {
        if !self.armed {
            return Err(WatchStateError::NotArmed);
        }
        through
            .validate()
            .map_err(WatchStateError::InvalidPosition)?;
        if self
            .observed
            .as_ref()
            .is_some_and(|previous| !position_follows(previous, &through))
        {
            self.full_scan = Some(FullScanReason::WatchInvalidated(
                Invalidation::CursorDiscontinuity,
            ));
        } else if self.full_scan.is_none() {
            self.full_scan = Some(FullScanReason::Periodic);
        }
        self.observed = Some(through);
        self.debounce_until_ms = Some(at_ms);
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
