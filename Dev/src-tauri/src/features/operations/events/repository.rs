use std::collections::VecDeque;
use std::sync::Mutex;

use syncdash::model::event::ProgressEvent;

use crate::contracts::events::{RunEventDto, RunEventPurposeDto};

const MAX_REPLAY_DIAGNOSTICS: usize = 256;

#[derive(Default)]
struct RunEventStore {
    run_id: Option<u64>,
    purpose: Option<RunEventPurposeDto>,
    next_sequence: u64,
    events: VecDeque<RunEventDto>,
}

#[derive(Default)]
pub(crate) struct RunEventRepository(Mutex<RunEventStore>);

impl RunEventRepository {
    pub(super) fn record(
        &self,
        run_id: u64,
        purpose: RunEventPurposeDto,
        ev: ProgressEvent,
    ) -> RunEventDto {
        let mut store = self.0.lock().unwrap();
        if store.run_id != Some(run_id) {
            store.run_id = Some(run_id);
            store.purpose = Some(purpose);
            store.events.clear();
        }
        store.next_sequence = store.next_sequence.saturating_add(1);
        let event = RunEventDto {
            sequence: store.next_sequence,
            run_id,
            purpose,
            ev,
        };
        compact_for(&mut store.events, &event.ev);
        if replayable(&event.ev) {
            store.events.push_back(event.clone());
            trim_diagnostics(&mut store.events);
        }
        event
    }

    pub(crate) fn replay(
        &self,
        purpose: RunEventPurposeDto,
        after_sequence: u64,
    ) -> Vec<RunEventDto> {
        let store = self.0.lock().unwrap();
        if store.purpose != Some(purpose) {
            return Vec::new();
        }
        store
            .events
            .iter()
            .filter(|event| event.sequence > after_sequence)
            .cloned()
            .collect()
    }
}

fn replayable(ev: &ProgressEvent) -> bool {
    !matches!(
        ev,
        ProgressEvent::ItemResult { .. }
            | ProgressEvent::Log {
                level: syncdash::model::event::LogLevel::Info,
                ..
            }
    )
}

fn compact_for(events: &mut VecDeque<RunEventDto>, next: &ProgressEvent) {
    match next {
        ProgressEvent::Progress { phase, .. } => events.retain(|event| {
            !matches!(&event.ev, ProgressEvent::Progress { phase: old, .. } if old == phase)
        }),
        ProgressEvent::Totals { phase, .. } => events.retain(|event| {
            !matches!(
                &event.ev,
                ProgressEvent::Progress { phase: old, .. }
                    | ProgressEvent::Totals { phase: old, .. }
                    if old == phase
            )
        }),
        ProgressEvent::Paused { .. } | ProgressEvent::Resumed { .. } => events.retain(|event| {
            !matches!(&event.ev, ProgressEvent::Paused { .. } | ProgressEvent::Resumed { .. })
        }),
        _ => {}
    }
}

fn diagnostic(ev: &ProgressEvent) -> bool {
    matches!(
        ev,
        ProgressEvent::Error { .. }
            | ProgressEvent::Log {
                level: syncdash::model::event::LogLevel::Warn
                    | syncdash::model::event::LogLevel::Error,
                ..
            }
    )
}

fn trim_diagnostics(events: &mut VecDeque<RunEventDto>) {
    while events.iter().filter(|event| diagnostic(&event.ev)).count() > MAX_REPLAY_DIAGNOSTICS {
        let Some(index) = events.iter().position(|event| diagnostic(&event.ev)) else {
            break;
        };
        events.remove(index);
    }
}

#[cfg(test)]
mod tests {
    use syncdash::model::event::{Phase, ProgressEvent};

    use super::*;

    #[test]
    fn replay_is_run_scoped_and_coalesces_progress_without_losing_boundaries() {
        let repository = RunEventRepository::default();
        repository.record(
            1,
            RunEventPurposeDto::Compare,
            ProgressEvent::PhaseStart {
                phase: Phase::ScanSource,
                ts_ms: 1,
                label: None,
                items_total: 0,
                bytes_total: 0,
            },
        );
        for done in [1, 2] {
            repository.record(
                1,
                RunEventPurposeDto::Compare,
                ProgressEvent::Progress {
                    phase: Phase::ScanSource,
                    ts_ms: done,
                    items_done: done,
                    items_total: 2,
                    bytes_done: done,
                    bytes_total: 2,
                    current_path: format!("{done}.txt"),
                },
            );
        }
        let replay = repository.replay(RunEventPurposeDto::Compare, 0);
        assert_eq!(replay.len(), 2);
        assert!(matches!(&replay[0].ev, ProgressEvent::PhaseStart { .. }));
        assert!(matches!(
            &replay[1].ev,
            ProgressEvent::Progress { items_done: 2, .. }
        ));
        assert!(repository.replay(RunEventPurposeDto::Apply, 0).is_empty());
        repository.record(
            2,
            RunEventPurposeDto::Apply,
            ProgressEvent::Paused { ts_ms: 3 },
        );
        assert!(repository.replay(RunEventPurposeDto::Compare, 0).is_empty());
        let next_run = repository.replay(RunEventPurposeDto::Apply, 0);
        assert_eq!(next_run.len(), 1);
        assert!(next_run[0].sequence > replay[1].sequence);
        assert!(repository
            .replay(RunEventPurposeDto::Apply, next_run[0].sequence)
            .is_empty());
    }
}
