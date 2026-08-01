import type { PhaseStatus } from './types/generated/PhaseStatus';

export interface CompareProgressEvent {
  kind: string;
  run_id: number;
  purpose: string;
  phase?: string;
  status?: PhaseStatus;
  reset?: boolean;
  label?: string | null;
  ts_ms?: number;
  items_done?: number;
  items_total?: number;
  bytes_done?: number;
  bytes_total?: number;
  action?: string;
  message?: string;
}

export interface CmpStage {
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

function blank(e: CompareProgressEvent): CmpStage {
  return {
    phase: e.phase!, label: e.label ?? '',
    itemsDone: 0, itemsTotal: 0, bytesDone: 0, bytesTotal: 0, rate: 0,
    active: true, done: false, failed: false, cancelled: false,
  };
}

/// Pure compare-progress transition. Keeping this outside React makes the parallel scan contract
/// executable in tests: one phase starting or progressing never completes the other phase.
export function reduceCompareStages(prev: CmpStage[], e: CompareProgressEvent, rate = 0): CmpStage[] {
  if (!e.phase) return prev;
  const current = prev.find((s) => s.phase === e.phase) ?? blank(e);
  let next: CmpStage;
  switch (e.kind) {
    case 'phase_start':
      next = {
        ...current, label: e.label ?? current.label,
        itemsDone: 0, itemsTotal: e.items_total ?? 0,
        bytesDone: 0, bytesTotal: e.bytes_total ?? 0, rate: 0,
        active: true, done: false, failed: false, cancelled: false,
      };
      break;
    case 'totals':
      next = {
        ...current,
        itemsDone: e.items_done ?? 0, itemsTotal: e.items_total ?? 0,
        bytesDone: e.bytes_done ?? 0, bytesTotal: e.bytes_total ?? 0, rate: 0,
      };
      break;
    case 'progress':
      next = {
        ...current, label: '',
        itemsDone: Math.max(current.itemsDone, e.items_done ?? 0),
        itemsTotal: Math.max(current.itemsTotal, e.items_total ?? 0),
        bytesDone: Math.max(current.bytesDone, e.bytes_done ?? 0),
        bytesTotal: Math.max(current.bytesTotal, e.bytes_total ?? 0),
        rate: Math.max(0, rate),
      };
      break;
    case 'phase_end':
      next = {
        ...current,
        itemsDone: e.items_done ?? current.itemsDone,
        itemsTotal: e.items_total ?? current.itemsTotal,
        bytesDone: e.bytes_done ?? current.bytesDone,
        bytesTotal: e.bytes_total ?? current.bytesTotal,
        active: false,
        done: e.status === 'completed',
        failed: e.status === 'failed',
        cancelled: e.status === 'cancelled',
      };
      break;
    default:
      return prev;
  }
  return prev.some((s) => s.phase === e.phase)
    ? prev.map((s) => (s.phase === e.phase ? next : s))
    : [...prev, next];
}
