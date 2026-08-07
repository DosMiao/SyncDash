import { eventLabel, eventPhase } from '#core/domain/runs/runEvents.ts';
import type { RunEventEnvelope } from '#core/domain/runs/runEvents.ts';

export type CompareProgressEvent = RunEventEnvelope;

export interface CompareStage {
  phase: string;
  label: string;
  itemsDone: number;
  itemsTotal: number;
  bytesDone: number;
  bytesTotal: number;
  rate: number;
  active: boolean;
  done: boolean;
  failed: boolean;
  cancelled: boolean;
}

function createCompareStage(event: CompareProgressEvent): CompareStage {
  return {
    phase: eventPhase(event)!, label: eventLabel(event) ?? '',
    itemsDone: 0, itemsTotal: 0, bytesDone: 0, bytesTotal: 0, rate: 0,
    active: true, done: false, failed: false, cancelled: false,
  };
}

/// Pure compare-progress transition. Keeping this outside React makes the parallel scan contract
/// executable in tests: one phase starting or progressing never completes the other phase.
export function reduceCompareStages(
  previousStages: CompareStage[],
  event: CompareProgressEvent,
  rate = 0,
): CompareStage[] {
  const phase = eventPhase(event);
  if (!phase) return previousStages;
  const current = previousStages.find((stage) => stage.phase === phase) ?? createCompareStage(event);
  let next: CompareStage;
  switch (event.kind) {
    case 'phase_start':
      next = {
        ...current, label: eventLabel(event) ?? current.label,
        itemsDone: 0, itemsTotal: event.items_total ?? 0,
        bytesDone: 0, bytesTotal: event.bytes_total ?? 0, rate: 0,
        active: true, done: false, failed: false, cancelled: false,
      };
      break;
    case 'totals':
      next = {
        ...current,
        itemsDone: event.items_done ?? 0, itemsTotal: event.items_total ?? 0,
        bytesDone: event.bytes_done ?? 0, bytesTotal: event.bytes_total ?? 0, rate: 0,
      };
      break;
    case 'progress':
      next = {
        ...current, label: '',
        itemsDone: Math.max(current.itemsDone, event.items_done ?? 0),
        itemsTotal: Math.max(current.itemsTotal, event.items_total ?? 0),
        bytesDone: Math.max(current.bytesDone, event.bytes_done ?? 0),
        bytesTotal: Math.max(current.bytesTotal, event.bytes_total ?? 0),
        rate: Math.max(0, rate),
      };
      break;
    case 'phase_end':
      next = {
        ...current,
        itemsDone: event.items_done ?? current.itemsDone,
        itemsTotal: event.items_total ?? current.itemsTotal,
        bytesDone: event.bytes_done ?? current.bytesDone,
        bytesTotal: event.bytes_total ?? current.bytesTotal,
        active: false,
        done: event.status === 'completed',
        failed: event.status === 'failed',
        cancelled: event.status === 'cancelled',
      };
      break;
    default:
      return previousStages;
  }
  return previousStages.some((stage) => stage.phase === event.phase)
    ? previousStages.map((stage) => (stage.phase === event.phase ? next : stage))
    : [...previousStages, next];
}

/// One error the engine reported mid-run, kept past the status line that first announced it.
export interface CompareRunFault {
  path: string;
  action: string;
  side: string;
  message: string;
}

/// Errors from the run in progress, with what was dropped stated rather than implied.
///
/// `total` counts every error the run reported; `retained` holds the ones kept for display. They
/// differ only past the cap, and the reader is told so — a list that silently stopped at 50 reads
/// as "50 problems" when it might be thousands.
export interface CompareRunFaults {
  total: number;
  retained: CompareRunFault[];
}

export const NO_COMPARE_RUN_FAULTS: CompareRunFaults = { total: 0, retained: [] };

/// Enough failing files to establish the shape of the problem; past this the count is the message.
const RETAINED_RUN_FAULTS = 50;

/// Pure fault accumulation, so "which errors survive the status line" is executable in tests.
///
/// `walk` is deliberately dropped: those events summarize the unread subtrees and skipped entries
/// that the finished plan header carries in full and in a better form, so keeping them here would
/// count the same problem twice — once live and once from the header. Everything else (a file that
/// changed while its content was being read, an unreadable file) appears in no header list at all,
/// and the path it names is the only place the user can learn which file it was.
export function recordCompareFault(
  previous: CompareRunFaults,
  event: CompareProgressEvent,
): CompareRunFaults {
  if (event.kind !== 'error' || event.action === 'walk') return previous;
  const fault: CompareRunFault = {
    path: event.path,
    action: event.action,
    side: event.side,
    message: event.message,
  };
  const known = previous.retained.some((seen) => (
    seen.path === fault.path && seen.action === fault.action && seen.message === fault.message
  ));
  return {
    total: previous.total + 1,
    retained: known || previous.retained.length >= RETAINED_RUN_FAULTS
      ? previous.retained
      : [...previous.retained, fault],
  };
}
