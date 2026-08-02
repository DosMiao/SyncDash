import type { RunEventEnvelope } from './runEvents';

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
    phase: event.phase!, label: event.label ?? '',
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
  if (!event.phase) return previousStages;
  const current = previousStages.find((stage) => stage.phase === event.phase) ?? createCompareStage(event);
  let next: CompareStage;
  switch (event.kind) {
    case 'phase_start':
      next = {
        ...current, label: event.label ?? current.label,
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
