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
fn periodic_verification_is_full_tree_and_commits_the_captured_position() {
    let initial = position("a", 0);
    let periodic = position("a", 7);
    let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(100, 512));
    finish_bootstrap(&mut trigger, initial);

    trigger.request_periodic(periodic.clone(), 500).unwrap();
    let ticket = trigger.next_work(500).unwrap().unwrap();
    assert_eq!(ticket.through, periodic);
    assert_eq!(
        ticket.coverage,
        WorkCoverage::FullTree {
            reason: FullScanReason::Periodic
        }
    );
    trigger.complete_success(ticket.id, |_| Ok(())).unwrap();
    assert_eq!(trigger.committed_position(), Some(&periodic));
}

#[test]
fn periodic_verification_does_not_downgrade_a_latched_invalidation() {
    let mut trigger = WatchTrigger::new(RigorPolicy::Quick, config(0, 512));
    finish_bootstrap(&mut trigger, position("a", 0));
    trigger
        .observe(
            ChangeBatch {
                through: position("a", 1),
                changed_paths: Vec::new(),
                invalidation: Some(Invalidation::CursorDiscontinuity),
            },
            1,
        )
        .unwrap();
    trigger.request_periodic(position("a", 1), 1).unwrap();
    assert_eq!(
        trigger.next_work(1).unwrap().unwrap().coverage,
        WorkCoverage::FullTree {
            reason: FullScanReason::WatchInvalidated(Invalidation::CursorDiscontinuity)
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
