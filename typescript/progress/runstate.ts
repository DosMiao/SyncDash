// Mutable run state for the progress window, plus the rate/ETA maths.
//
// Deliberately a plain mutable object rather than React state: events arrive up to ten times a second
// and the sample series reaches thousands of entries. The window re-renders on its own cadence (500 ms
// for the readouts, 100 ms for the graphs), which is where FFS puts the throttle.

import type { RunEventEnvelope } from '../core/runEvents';
import type { Phase } from '../core/types/generated/Phase';
import type { PhaseStatus } from '../core/types/generated/PhaseStatus';

/// The engine's phase names, not a copy of them. A phase added in Rust becomes a missing key in
/// PHASE_LABEL below — a compile error, rather than a blank cell at runtime.
export type PhaseName = Phase;

export const PHASE_LABEL: Record<PhaseName, string> = {
  'scan-source': 'Scan source',
  'scan-target': 'Scan target',
  'compare': 'Compare',
  'apply': 'Sync',
  'pack': 'Pack',
  'ship': 'Transfer',
  'verify': 'Verify',
  'refresh': 'Refresh archive',
  'archive': 'Save archive',
};

/// The `run-progress` payload: the engine's `ProgressEvent` plus the two fields the Tauri shell
/// wraps it in. Every field below is optional because the arms of that union are read structurally
/// here rather than narrowed on `kind` — but the *names and spellings* come from the generated
/// type, so a renamed or removed variant is a compile error rather than a blank readout.
export type EventKind = RunEventEnvelope['kind'];
export type RunEv = RunEventEnvelope;

/// t = active milliseconds (paused time removed)
export interface Sample { t: number; b: number; i: number }

export interface StageRow { phase: PhaseName; detail: string; active: boolean; done: boolean; failed?: boolean; cancelled?: boolean }
export interface ErrRow { path: string; action: string; side: string; message: string; warning: boolean }

export function startStage(row: StageRow): void {
  row.active = true;
  row.done = false;
}

export function endStage(row: StageRow, status: PhaseStatus | undefined): void {
  row.active = false;
  row.failed = !!row.failed || status === 'failed';
  row.cancelled = !!row.cancelled || status === 'cancelled';
  row.done = status === 'completed' && !row.failed && !row.cancelled;
}

export interface RunState {
  runId: number;
  running: boolean;
  /// ts of the first event in this run
  t0: number;
  /// authoritative total from the engine (refreshed by Resumed/Summary)
  pausedMs: number;
  /// start of the current pause (local estimate)
  pausedSince: number;
  phase: PhaseName | null;
  /// has entered apply/pack/ship → graph mode
  applying: boolean;
  totals: { items: number; bytes: number };
  dones: { items: number; bytes: number };
  samples: Sample[];
  currentPath: string;
  stages: StageRow[];
  errors: ErrRow[];
  summary: RunEv | null;
  closeAfterStop: boolean;
}

export function newRunState(id = -1, ts = 0): RunState {
  return {
    runId: id, running: id >= 0, t0: ts,
    pausedMs: 0, pausedSince: 0,
    phase: null, applying: false,
    totals: { items: 0, bytes: 0 },
    dones: { items: 0, bytes: 0 },
    samples: [], currentPath: '',
    stages: [], errors: [],
    summary: null, closeAfterStop: false,
  };
}

export function activeNow(s: RunState): number {
  const now = Date.now();
  const livePause = s.pausedSince ? now - s.pausedSince : 0;
  return Math.max(0, now - s.t0 - s.pausedMs - livePause);
}

export function percent(s: RunState): number {
  if (s.summary && !s.summary.cancelled && (s.summary.errors ?? 0) === 0) return 100;
  const denom = s.totals.bytes + s.totals.items;
  if (denom <= 0) return 0;
  const raw = Math.max(0, (s.dones.bytes + s.dones.items) * 100 / denom);
  // Bytes can finish before fsync, verify, preservation, commit, and the remaining metadata ops.
  // A live 100 is therefore a false terminal claim; Summary is the one run-complete boundary.
  if (s.running && !s.summary) return Math.min(99, Math.floor(raw));
  return Math.min(100, raw);
}

/// Differencing the sample series over a sliding window. When the window is not yet full, use the actual
/// span (same as FFS) rather than reporting nothing.
export function windowRate(s: RunState, windowMs: number): { bps: number; ips: number } | null {
  const { samples } = s;
  if (samples.length < 2) return null;
  const last = samples[samples.length - 1];
  const cut = last.t - windowMs;
  let base = samples[0];
  for (let k = samples.length - 2; k >= 0; k--) {
    if (samples[k].t <= cut) { base = samples[k]; break; }
  }
  const dt = last.t - base.t;
  if (dt < 200) return null;
  return { bps: (last.b - base.b) * 1000 / dt, ips: (last.i - base.i) * 1000 / dt };
}
