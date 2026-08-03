import type { RunSummaryEvent } from '#core/domain/runs/runEvents.ts';
import type { RunEventEnvelope } from '#core/domain/runs/runEvents.ts';
import type { Phase } from '#core/types/generated/Phase.ts';
import type { PhaseStatus } from '#core/types/generated/PhaseStatus.ts';

export type RunPhase = Phase;
export type RunProgressEvent = RunEventEnvelope;

export const PHASE_LABELS: Record<RunPhase, string> = {
  'scan-source': 'Scan source',
  'scan-target': 'Scan target',
  'compare': 'Compare',
  'apply': 'Sync',
  'pack': 'Pack',
  'ship': 'Transfer',
  'refresh': 'Refresh archive',
  'archive': 'Save archive',
};

export interface ProgressRateSample {
  activeElapsedMs: number;
  bytesDone: number;
  itemsDone: number;
}

export interface ProgressStage {
  phase: RunPhase;
  detail: string;
  active: boolean;
  done: boolean;
  failed?: boolean;
  cancelled?: boolean;
}

export interface ProgressIssue {
  path: string;
  action: string;
  side: string;
  message: string;
  warning: boolean;
}

export function startStage(stage: ProgressStage): void {
  stage.active = true;
  stage.done = false;
}

export function endStage(stage: ProgressStage, status: PhaseStatus | undefined): void {
  stage.active = false;
  stage.failed = !!stage.failed || status === 'failed';
  stage.cancelled = !!stage.cancelled || status === 'cancelled';
  stage.done = status === 'completed' && !stage.failed && !stage.cancelled;
}

export interface RunState {
  runId: number;
  running: boolean;
  startedAtMs: number;
  pausedMs: number;
  pausedSince: number;
  phase: RunPhase | null;
  applying: boolean;
  totals: { items: number; bytes: number };
  completed: { items: number; bytes: number };
  samples: ProgressRateSample[];
  currentPath: string;
  stages: ProgressStage[];
  errors: ProgressIssue[];
  summary: RunSummaryEvent | null;
  closeAfterStop: boolean;
}

export function newRunState(runId = -1, startedAtMs = 0): RunState {
  return {
    runId,
    running: runId >= 0,
    startedAtMs,
    pausedMs: 0,
    pausedSince: 0,
    phase: null,
    applying: false,
    totals: { items: 0, bytes: 0 },
    completed: { items: 0, bytes: 0 },
    samples: [],
    currentPath: '',
    stages: [],
    errors: [],
    summary: null,
    closeAfterStop: false,
  };
}

export function activeElapsedMs(state: RunState): number {
  const now = Date.now();
  const currentPauseMs = state.pausedSince ? now - state.pausedSince : 0;
  return Math.max(0, now - state.startedAtMs - state.pausedMs - currentPauseMs);
}

export function completionPercent(state: RunState): number {
  if (state.summary && !state.summary.cancelled && state.summary.errors === 0) return 100;
  const totalWork = state.totals.bytes + state.totals.items;
  if (totalWork <= 0) return 0;
  const rawPercent = Math.max(
    0,
    (state.completed.bytes + state.completed.items) * 100 / totalWork,
  );
  // Byte transfer can reach its total before seal, sync, verification, preservation, and publish.
  if (state.running && !state.summary) return Math.min(99, Math.floor(rawPercent));
  return Math.min(100, rawPercent);
}

export interface ProgressRate {
  bytesPerSecond: number;
  itemsPerSecond: number;
}

export function calculateWindowRate(state: RunState, windowMs: number): ProgressRate | null {
  const { samples } = state;
  if (samples.length < 2) return null;
  const latest = samples[samples.length - 1];
  const windowStartMs = latest.activeElapsedMs - windowMs;
  let baseline = samples[0];
  for (let index = samples.length - 2; index >= 0; index--) {
    if (samples[index].activeElapsedMs <= windowStartMs) {
      baseline = samples[index];
      break;
    }
  }
  const elapsedMs = latest.activeElapsedMs - baseline.activeElapsedMs;
  if (elapsedMs < 200) return null;
  return {
    bytesPerSecond: (latest.bytesDone - baseline.bytesDone) * 1000 / elapsedMs,
    itemsPerSecond: (latest.itemsDone - baseline.itemsDone) * 1000 / elapsedMs,
  };
}
