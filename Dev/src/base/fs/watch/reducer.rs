//! Pure reduction of raw FSEvents records into cursors, triggers, and invalidations.
//!
//! Only the macOS backend produces these records, so the module is absent on platforms without
//! FSEvents. It stays compiled under `test` everywhere because the reduction rules — dropped
//! events, journal resets, and device-path translation — are deliberately free of timing and of any
//! mounted volume, and they keep that coverage on every platform.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    InvalidationKind, RootCursor, TriggerBatch, TriggerPath, WatchInvalidation, WatchPosition,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct RawFlags {
    pub must_scan_subdirectories: bool,
    pub user_dropped: bool,
    pub kernel_dropped: bool,
    pub event_ids_wrapped: bool,
    pub history_done: bool,
    pub root_changed: bool,
    pub unmounted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RawEvent {
    pub event_id: u64,
    /// FSEvents per-device path, relative to the root of that volume.
    pub device_relative_path: String,
    pub flags: RawFlags,
}

#[derive(Clone, Debug)]
pub(super) struct StreamSeed {
    pub stream: String,
    pub device_relative_root: String,
    pub cursor: RootCursor,
}

#[derive(Clone, Debug)]
struct StreamState {
    device_relative_root: String,
    cursor: RootCursor,
    reset_generation: u64,
}

/// Pure FSEvents batch reducer shared by the native callback and scripted unit tests.
pub(super) struct EventReducer {
    streams: BTreeMap<String, StreamState>,
}

impl EventReducer {
    pub fn new(seeds: impl IntoIterator<Item = StreamSeed>) -> Result<Self, String> {
        let mut streams = BTreeMap::new();
        for seed in seeds {
            if seed.stream.trim().is_empty() {
                return Err("watch stream name cannot be empty".into());
            }
            if seed.cursor.epoch.trim().is_empty() {
                return Err(format!("watch stream {:?} has an empty epoch", seed.stream));
            }
            let stream = seed.stream.clone();
            if streams
                .insert(
                    seed.stream,
                    StreamState {
                        device_relative_root: normalize_device_path(&seed.device_relative_root),
                        cursor: seed.cursor,
                        reset_generation: 0,
                    },
                )
                .is_some()
            {
                return Err(format!("duplicate watch stream {stream:?}"));
            }
        }
        if streams.is_empty() {
            return Err("at least one watch stream is required".into());
        }
        Ok(Self { streams })
    }

    pub fn position(&self) -> WatchPosition {
        WatchPosition {
            streams: self
                .streams
                .iter()
                .map(|(name, state)| (name.clone(), state.cursor.clone()))
                .collect(),
        }
    }

    pub fn reduce(
        &mut self,
        stream: &str,
        events: impl IntoIterator<Item = RawEvent>,
    ) -> Result<Option<TriggerBatch>, String> {
        let state = self
            .streams
            .get_mut(stream)
            .ok_or_else(|| format!("event received for unknown watch stream {stream:?}"))?;
        let mut changed_paths = BTreeSet::new();
        let mut invalidations = BTreeSet::new();

        for event in events {
            let id_regressed = event.event_id != 0 && event.event_id < state.cursor.event_id;
            if event.flags.event_ids_wrapped || id_regressed {
                state.reset_generation = state.reset_generation.saturating_add(1);
                let base = state
                    .cursor
                    .journal_uuid
                    .as_deref()
                    .unwrap_or(state.cursor.epoch.as_str());
                state.cursor.epoch = format!("fsevents:{base}:reset-{}", state.reset_generation);
                state.cursor.event_id = event.event_id;
                invalidations.insert(WatchInvalidation {
                    stream: stream.into(),
                    kind: InvalidationKind::EventIdsWrapped,
                });
            } else if event.event_id != 0 {
                state.cursor.event_id = state.cursor.event_id.max(event.event_id);
            }

            push_flag_invalidations(stream, event.flags, &mut invalidations);
            if event.flags.history_done {
                continue;
            }

            match path_below_root(&state.device_relative_root, &event.device_relative_path) {
                PathRelation::Descendant(path) => {
                    changed_paths.insert(TriggerPath {
                        stream: stream.into(),
                        path,
                    });
                }
                PathRelation::Root => {
                    invalidations.insert(WatchInvalidation {
                        stream: stream.into(),
                        kind: InvalidationKind::WholeRootChanged,
                    });
                }
                PathRelation::Outside => {
                    invalidations.insert(WatchInvalidation {
                        stream: stream.into(),
                        kind: InvalidationKind::PathOutsideRoot,
                    });
                }
            }
        }

        if changed_paths.is_empty() && invalidations.is_empty() {
            return Ok(None);
        }
        Ok(Some(TriggerBatch {
            position: self.position(),
            changed_paths: changed_paths.into_iter().collect(),
            invalidations: invalidations.into_iter().collect(),
        }))
    }
}

fn push_flag_invalidations(
    stream: &str,
    flags: RawFlags,
    invalidations: &mut BTreeSet<WatchInvalidation>,
) {
    let mut push = |kind| {
        invalidations.insert(WatchInvalidation {
            stream: stream.into(),
            kind,
        });
    };
    if flags.user_dropped {
        push(InvalidationKind::UserDropped);
    }
    if flags.kernel_dropped {
        push(InvalidationKind::KernelDropped);
    }
    if flags.must_scan_subdirectories && !flags.user_dropped && !flags.kernel_dropped {
        push(InvalidationKind::MustScanSubdirectories);
    }
    if flags.root_changed {
        push(InvalidationKind::RootChanged);
    }
    if flags.unmounted {
        push(InvalidationKind::Unmounted);
    }
}

#[derive(Debug, Eq, PartialEq)]
enum PathRelation {
    Descendant(String),
    Root,
    Outside,
}

fn path_below_root(root: &str, event: &str) -> PathRelation {
    let root = normalize_device_path(root);
    let event = normalize_device_path(event);
    if event == root {
        return PathRelation::Root;
    }
    if root.is_empty() {
        return if event.is_empty() {
            PathRelation::Root
        } else {
            PathRelation::Descendant(event)
        };
    }
    let Some(relative) = event
        .strip_prefix(&root)
        .and_then(|tail| tail.strip_prefix('/'))
    else {
        return PathRelation::Outside;
    };
    if relative.is_empty() {
        PathRelation::Root
    } else {
        PathRelation::Descendant(relative.into())
    }
}

fn normalize_device_path(path: &str) -> String {
    let path = path.trim_matches('/');
    if path == "." {
        String::new()
    } else {
        path.strip_prefix("./").unwrap_or(path).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::watch::{SOURCE_STREAM, TARGET_STREAM};

    fn cursor(epoch: &str, event_id: u64) -> RootCursor {
        RootCursor {
            journal_uuid: Some(epoch.into()),
            epoch: format!("fsevents:{epoch}"),
            event_id,
        }
    }

    fn reducer() -> EventReducer {
        EventReducer::new([
            StreamSeed {
                stream: SOURCE_STREAM.into(),
                device_relative_root: "Users/me/Code".into(),
                cursor: cursor("source-uuid", 10),
            },
            StreamSeed {
                stream: TARGET_STREAM.into(),
                device_relative_root: "Code".into(),
                cursor: cursor("target-uuid", 20),
            },
        ])
        .unwrap()
    }

    #[test]
    fn reducer_qualifies_paths_and_carries_both_root_cursors() {
        let mut reducer = reducer();
        let batch = reducer
            .reduce(
                SOURCE_STREAM,
                [RawEvent {
                    event_id: 11,
                    device_relative_path: "Users/me/Code/src/lib.rs".into(),
                    flags: RawFlags::default(),
                }],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            batch.changed_paths,
            vec![TriggerPath {
                stream: SOURCE_STREAM.into(),
                path: "src/lib.rs".into()
            }]
        );
        assert_eq!(batch.position.streams[SOURCE_STREAM].event_id, 11);
        assert_eq!(batch.position.streams[TARGET_STREAM].event_id, 20);
    }

    #[test]
    fn dropped_and_coalesced_events_become_explicit_invalidations() {
        let mut reducer = reducer();
        let batch = reducer
            .reduce(
                TARGET_STREAM,
                [RawEvent {
                    event_id: 21,
                    device_relative_path: "Code".into(),
                    flags: RawFlags {
                        must_scan_subdirectories: true,
                        user_dropped: true,
                        ..RawFlags::default()
                    },
                }],
            )
            .unwrap()
            .unwrap();
        assert!(batch.invalidations.contains(&WatchInvalidation {
            stream: TARGET_STREAM.into(),
            kind: InvalidationKind::UserDropped,
        }));
        assert!(batch.invalidations.contains(&WatchInvalidation {
            stream: TARGET_STREAM.into(),
            kind: InvalidationKind::WholeRootChanged,
        }));
    }

    #[test]
    fn wrapped_or_regressed_ids_rotate_the_epoch_and_never_look_monotonic() {
        let mut reducer = reducer();
        let batch = reducer
            .reduce(
                SOURCE_STREAM,
                [RawEvent {
                    event_id: 2,
                    device_relative_path: "Users/me/Code/a".into(),
                    flags: RawFlags::default(),
                }],
            )
            .unwrap()
            .unwrap();
        let current = &batch.position.streams[SOURCE_STREAM];
        assert_eq!(current.event_id, 2);
        assert_ne!(current.epoch, "fsevents:source-uuid");
        assert!(batch.invalidations.contains(&WatchInvalidation {
            stream: SOURCE_STREAM.into(),
            kind: InvalidationKind::EventIdsWrapped,
        }));
    }

    #[test]
    fn root_change_id_zero_does_not_move_the_cursor_backwards() {
        let mut reducer = reducer();
        let batch = reducer
            .reduce(
                SOURCE_STREAM,
                [RawEvent {
                    event_id: 0,
                    device_relative_path: "Users/me/Code".into(),
                    flags: RawFlags {
                        root_changed: true,
                        ..RawFlags::default()
                    },
                }],
            )
            .unwrap()
            .unwrap();
        assert_eq!(batch.position.streams[SOURCE_STREAM].event_id, 10);
        assert!(batch.invalidations.contains(&WatchInvalidation {
            stream: SOURCE_STREAM.into(),
            kind: InvalidationKind::RootChanged,
        }));
    }

    #[test]
    fn paths_outside_the_registered_root_never_become_incremental_hints() {
        let mut reducer = reducer();
        let batch = reducer
            .reduce(
                SOURCE_STREAM,
                [RawEvent {
                    event_id: 11,
                    device_relative_path: "Users/other/file".into(),
                    flags: RawFlags::default(),
                }],
            )
            .unwrap()
            .unwrap();
        assert!(batch.changed_paths.is_empty());
        assert_eq!(
            batch.invalidations,
            vec![WatchInvalidation {
                stream: SOURCE_STREAM.into(),
                kind: InvalidationKind::PathOutsideRoot
            }]
        );
    }
}
