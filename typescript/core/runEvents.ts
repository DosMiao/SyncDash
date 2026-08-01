import type { ItemOutcome } from './types/generated/ItemOutcome';
import type { LogLevel } from './types/generated/LogLevel';
import type { Phase } from './types/generated/Phase';
import type { PhaseStatus } from './types/generated/PhaseStatus';
import type { ProgressEvent } from './types/generated/ProgressEvent';

export interface RunEventEnvelope {
  sequence: number;
  run_id: number;
  purpose: 'compare' | 'apply';
  kind: ProgressEvent['kind'];
  ts_ms: number;
  phase?: Phase;
  status?: PhaseStatus;
  reset?: boolean;
  label?: string | null;
  items_total?: number;
  bytes_total?: number;
  items_done?: number;
  bytes_done?: number;
  current_path?: string;
  path?: string;
  action?: string;
  side?: string;
  message?: string;
  level?: LogLevel;
  scope?: string;
  outcome?: ItemOutcome;
  bytes?: number;
  ms?: number;
  paused_ms?: number;
  done?: number;
  skipped?: number;
  errors?: number;
  elapsed_ms?: number;
  cancelled?: boolean;
}

export function mergeRunEventReplay(
  replay: RunEventEnvelope[],
  queued: RunEventEnvelope[],
): RunEventEnvelope[] {
  const ordered = [...replay, ...queued]
    .sort((left, right) => left.sequence - right.sequence);
  return ordered.filter((event, index) => (
    index === 0 || event.sequence !== ordered[index - 1].sequence
  ));
}
