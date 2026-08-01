// Run-log parsing. One view per artifact — Events (run.jsonl) · Errors (errors.jsonl) · Items
// (items.jsonl) · Plan (plan.jsonl) — and all four normalize to LogRow so one renderer covers them.
// The plan view earns its keep: after an interrupted run, the rows missing from items show up only
// against the plan.

import { humanSize } from './format';
import type { LogLevel } from './types/generated/LogLevel';
import type { RunRecord } from './types/generated/RunRecord';

export type LogView = 'run' | 'errors' | 'items' | 'plan';

/// From the engine: the same three levels the sinks filter on. A fourth level added in Rust must
/// reach the level picker, and this is what makes that a compile error rather than a silent gap.
export type { LogLevel };

export const LP_VIEWS: [LogView, string][] = [
  ['run', 'Events'], ['errors', 'Errors'], ['items', 'Items'], ['plan', 'Plan'],
];
export const LP_LEVELS: [LogLevel | 'all', string][] = [
  ['all', 'All'], ['info', 'Info'], ['warn', 'Warn'], ['error', 'Error'],
];
export const LOG_LEVEL_LABEL: Record<LogLevel, string> = { info: 'Info', warn: 'Warn', error: 'Error' };

const OUTCOME: Record<string, string> = {
  ok: '✓ ok', failed: '✗ failed', kept: '⊘ kept', cancelled: '■ not run',
};

/** One parsed log line — all four artifacts normalize to this shape and share a single renderer */
export interface LogRow {
  ts: number;
  level: LogLevel;
  scope: string;
  text: string;
  /** Search matches the path as well */
  hay: string;
}

/** How one run reads in the dropdown. An interrupted run must stand out at a glance — it is exactly the one worth opening. */
export function runLabel(r: RunRecord): string {
  const when = new Date(r.ts_ms).toLocaleString();
  if (r.ops_found != null) return `${when}  ${r.job}  compare · ${r.ops_found} found`;
  const bad = r.errors ? ` · ${r.errors} errors` : '';
  const warn = r.warnings ? ` · ${r.warnings} warnings` : '';
  const state = r.finished === false ? ' · ⚠ interrupted' : r.cancelled ? ' · cancelled' : '';
  return `${when}  ${r.job}  ${r.done} items${bad}${warn}${state}`;
}

/** One JSONL line → LogRow. Unrecognized lines are shown verbatim, never swallowed. */
export function parseLogLine(raw: string, view: LogView): LogRow | null {
  let v: Record<string, unknown>;
  try { v = JSON.parse(raw); } catch { return { ts: 0, level: 'info', scope: '?', text: raw, hay: raw }; }
  const ts = Number(v.ts_ms ?? 0);
  if (view === 'plan') {
    const op = v.op as Record<string, unknown> | undefined;
    if (!op) return null;
    const t = `${op.action}  ${op.path}  (${op.side}) ${op.reason ?? ''}`;
    return { ts: Number(op.mtime_ms ?? 0), level: 'info', scope: 'plan', text: t, hay: t };
  }
  switch (v.kind) {
    case 'log':
      return { ts, level: (v.level as LogLevel) ?? 'info', scope: String(v.scope ?? ''), text: String(v.message ?? ''), hay: String(v.message ?? '') };
    case 'error': {
      const t = `${v.action} ${v.path} (${v.side}): ${v.message}`;
      return { ts, level: 'error', scope: String(v.phase ?? 'apply'), text: t, hay: t };
    }
    case 'item_result': {
      const oc = String(v.outcome);
      const lvl: LogLevel = oc === 'failed' ? 'error' : oc === 'ok' ? 'info' : 'warn';
      const t = `${OUTCOME[oc] ?? oc}  ${v.action} ${v.path}  ${humanSize(Number(v.bytes)) || '0 B'}${Number(v.ms) ? ` · ${v.ms}ms` : ''}`;
      return { ts, level: lvl, scope: String(v.side ?? ''), text: t, hay: String(v.path ?? '') };
    }
    case 'phase_start':
      return { ts, level: 'info', scope: 'phase', text: `▶ ${v.phase}${v.label ? `  ${v.label}` : ''}`, hay: String(v.label ?? '') };
    case 'phase_end': {
      const mark = v.status === 'completed' ? '✓' : v.status === 'cancelled' ? '■' : '✕';
      const level = v.status === 'failed' ? 'error' : 'info';
      return { ts, level, scope: 'phase', text: `${mark} ${v.phase} · ${v.status}`, hay: String(v.phase ?? '') };
    }
    case 'summary': {
      const t = `■ Done: ${v.done} run, ${v.skipped} skipped, ${v.errors} errors · ${humanSize(Number(v.bytes_done)) || '0 B'} · ${((Number(v.elapsed_ms)) / 1000).toFixed(1)}s${v.cancelled ? ' · cancelled' : ''}`;
      return { ts, level: Number(v.errors) ? 'error' : 'info', scope: 'final', text: t, hay: t };
    }
    default:
      return { ts, level: 'info', scope: String(v.kind ?? '?'), text: raw, hay: raw };
  }
}
