//! Sampling cadence for legacy walk and hash progress callbacks.

const HASH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);
pub(super) const WALK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

pub(super) fn hash_sample_due(stop: &std::sync::mpsc::Receiver<()>) -> bool {
    matches!(
        stop.recv_timeout(HASH_INTERVAL),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    )
}

pub(super) fn walk_sample_due(
    last: &mut Option<std::time::Duration>,
    now: std::time::Duration,
) -> bool {
    let due = last.map_or(true, |previous| {
        now.saturating_sub(previous) >= WALK_INTERVAL
    });
    if due {
        *last = Some(now);
    }
    due
}
