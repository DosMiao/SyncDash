use std::sync::atomic::{AtomicU64, Ordering};

use syncdash::model::event::Phase;

fn phase_slot(phase: Phase) -> usize {
    match phase {
        Phase::ScanSource => 0,
        Phase::ScanTarget => 1,
        Phase::Compare => 2,
        Phase::Apply => 3,
        Phase::Pack => 4,
        Phase::Ship => 5,
        Phase::Verify => 6,
        Phase::Refresh => 7,
        Phase::Archive => 8,
    }
}

pub(super) struct ProgressThrottle {
    last_ms: [AtomicU64; 9],
}

impl Default for ProgressThrottle {
    fn default() -> Self {
        Self {
            last_ms: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl ProgressThrottle {
    pub(super) fn allows(&self, phase: Phase, ts_ms: u64) -> bool {
        self.last_ms[phase_slot(phase)]
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
                (ts_ms.saturating_sub(last) >= 100).then_some(ts_ms)
            })
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_phases_are_throttled_independently() {
        let throttle = ProgressThrottle::default();
        assert!(throttle.allows(Phase::ScanSource, 1_000));
        assert!(throttle.allows(Phase::ScanTarget, 1_001));
        assert!(!throttle.allows(Phase::ScanSource, 1_050));
        assert!(throttle.allows(Phase::ScanSource, 1_100));
    }
}
