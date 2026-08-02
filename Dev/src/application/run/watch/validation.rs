//! Validation leaves for native cursor continuity and relative path hints.

use crate::fs::watch::WatchPosition;

pub(super) fn position_follows(previous: &WatchPosition, next: &WatchPosition) -> bool {
    previous.streams.len() == next.streams.len()
        && previous.streams.iter().all(|(name, old)| {
            next.streams.get(name).is_some_and(|new| {
                new.journal_uuid == old.journal_uuid
                    && new.epoch == old.epoch
                    && new.event_id >= old.event_id
            })
        })
}

pub(super) fn safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains('\\')
        && path.as_bytes().get(1) != Some(&b':')
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}
