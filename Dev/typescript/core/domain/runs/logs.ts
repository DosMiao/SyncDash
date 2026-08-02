import { humanDuration, humanSize } from '#core/shared/format.ts';
import type { LogArtifactKind } from '#core/types/generated/LogArtifactKind.ts';
import type { LogLevel } from '#core/types/generated/LogLevel.ts';
import type { RunRecord } from '#core/types/generated/RunRecord.ts';

export type LogArtifactView = Exclude<LogArtifactKind, 'summary'>;
export type LogLevelFilter = LogLevel | 'all';

export type { LogLevel };

export const LOG_ARTIFACT_VIEW_OPTIONS: ReadonlyArray<readonly [LogArtifactView, string]> = [
  ['run', 'Events'], ['errors', 'Errors'], ['items', 'Items'], ['plan', 'Plan'],
];
export const LOG_LEVEL_FILTER_OPTIONS: ReadonlyArray<readonly [LogLevelFilter, string]> = [
  ['all', 'All'], ['info', 'Info'], ['warn', 'Warn'], ['error', 'Error'],
];
export const LOG_LEVEL_LABELS: Record<LogLevel, string> = {
  info: 'Info',
  warn: 'Warn',
  error: 'Error',
};

const ITEM_OUTCOME_LABELS: Record<string, string> = {
  ok: '✓ ok',
  failed: '✗ failed',
  kept: '⊘ kept',
  cancelled: '■ not run',
};

export interface LogRow {
  timestampMs: number;
  level: LogLevel;
  scope: string;
  message: string;
  searchText: string;
}

export function formatRunLabel(record: RunRecord): string {
  const timestamp = new Date(record.ts_ms).toLocaleString();
  if (record.ops_found != null) {
    return `${timestamp}  ${record.subject.job_name}  compare · ${record.ops_found} found`;
  }
  const errors = record.errors ? ` · ${record.errors} errors` : '';
  const warnings = record.warnings ? ` · ${record.warnings} warnings` : '';
  const state = record.finished === false
    ? ' · ⚠ interrupted'
    : record.cancelled
      ? ' · cancelled'
      : '';
  return `${timestamp}  ${record.subject.job_name}  ${record.done} items${errors}${warnings}${state}`;
}

export function runHasEventArtifact(record: RunRecord): boolean {
  return record.artifacts.kind === 'directory' || record.artifacts.kind === 'legacy_file';
}

export function runHasArtifactSet(record: RunRecord): boolean {
  return record.artifacts.kind === 'directory';
}

function recordField(record: Record<string, unknown>, field: string): unknown {
  if (!(field in record)) throw new Error(`missing '${field}'`);
  return record[field];
}

function stringField(record: Record<string, unknown>, field: string): string {
  const value = recordField(record, field);
  if (typeof value !== 'string') throw new Error(`'${field}' must be a string`);
  return value;
}

function optionalStringField(record: Record<string, unknown>, field: string): string | null {
  const value = record[field];
  if (value === undefined || value === null) return null;
  if (typeof value !== 'string') throw new Error(`'${field}' must be a string when present`);
  return value;
}

function numberField(record: Record<string, unknown>, field: string): number {
  const value = recordField(record, field);
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
    throw new Error(`'${field}' must be a non-negative safe integer`);
  }
  return value;
}

function optionalIntegerField(record: Record<string, unknown>, field: string): number | null {
  const value = record[field];
  if (value === undefined || value === null) return null;
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) {
    throw new Error(`'${field}' must be a safe integer when present`);
  }
  return value;
}

function booleanField(record: Record<string, unknown>, field: string): boolean {
  const value = recordField(record, field);
  if (typeof value !== 'boolean') throw new Error(`'${field}' must be a boolean`);
  return value;
}

function parseFailure(rawLine: string, error: unknown): LogRow {
  const detail = error instanceof Error ? error.message : String(error);
  const message = `Malformed JSON log record: ${detail}`;
  return {
    timestampMs: 0,
    level: 'error',
    scope: 'parse',
    message,
    searchText: `${message} ${rawLine}`,
  };
}

function isLogLevel(value: unknown): value is LogLevel {
  return value === 'info' || value === 'warn' || value === 'error';
}

function parsePlanRow(record: Record<string, unknown>, rawLine: string): LogRow {
  const rawOperation = recordField(record, 'op');
  if (rawOperation === null || Array.isArray(rawOperation) || typeof rawOperation !== 'object') {
    throw new Error("'op' must be an object");
  }
  const operation = rawOperation as Record<string, unknown>;
  const action = stringField(operation, 'action');
  const path = stringField(operation, 'path');
  const side = stringField(operation, 'side');
  const reason = optionalStringField(operation, 'reason') ?? '';
  const message = `${action}  ${path}  (${side})${reason ? ` ${reason}` : ''}`;
  return {
    timestampMs: optionalIntegerField(operation, 'mtime_ms') ?? 0,
    level: 'info',
    scope: 'plan',
    message,
    searchText: `${message} ${rawLine}`,
  };
}

function parseRunEvent(record: Record<string, unknown>, rawLine: string): LogRow {
  const kind = stringField(record, 'kind');
  const timestampMs = numberField(record, 'ts_ms');
  switch (kind) {
    case 'log': {
      const level = recordField(record, 'level');
      if (!isLogLevel(level)) throw new Error("'level' is not a supported log level");
      const scope = stringField(record, 'scope');
      const message = stringField(record, 'message');
      return { timestampMs, level, scope, message, searchText: `${scope} ${message}` };
    }
    case 'error': {
      const phase = stringField(record, 'phase');
      const action = stringField(record, 'action');
      const path = stringField(record, 'path');
      const side = stringField(record, 'side');
      const detail = stringField(record, 'message');
      const message = `${action} ${path} (${side}): ${detail}`;
      return { timestampMs, level: 'error', scope: phase, message, searchText: message };
    }
    case 'item_result': {
      const outcome = stringField(record, 'outcome');
      if (!(outcome in ITEM_OUTCOME_LABELS)) throw new Error("'outcome' is not supported");
      const action = stringField(record, 'action');
      const path = stringField(record, 'path');
      const side = stringField(record, 'side');
      const bytes = numberField(record, 'bytes');
      const elapsedMs = numberField(record, 'ms');
      const level: LogLevel = outcome === 'failed' ? 'error' : outcome === 'ok' ? 'info' : 'warn';
      const message = `${ITEM_OUTCOME_LABELS[outcome]}  ${action} ${path}  ${humanSize(bytes)}${elapsedMs ? ` · ${elapsedMs}ms` : ''}`;
      return { timestampMs, level, scope: side, message, searchText: `${path} ${message}` };
    }
    case 'phase_start': {
      const phase = stringField(record, 'phase');
      const label = optionalStringField(record, 'label');
      numberField(record, 'items_total');
      numberField(record, 'bytes_total');
      const message = `▶ ${phase}${label ? `  ${label}` : ''}`;
      return { timestampMs, level: 'info', scope: 'phase', message, searchText: message };
    }
    case 'totals': {
      const phase = stringField(record, 'phase');
      const itemsTotal = numberField(record, 'items_total');
      const bytesTotal = numberField(record, 'bytes_total');
      numberField(record, 'items_done');
      numberField(record, 'bytes_done');
      booleanField(record, 'reset');
      const message = `Totals · ${phase} · ${itemsTotal} items · ${humanSize(bytesTotal)}`;
      return { timestampMs, level: 'info', scope: 'phase', message, searchText: message };
    }
    case 'phase_end': {
      const phase = stringField(record, 'phase');
      const status = stringField(record, 'status');
      if (status !== 'completed' && status !== 'cancelled' && status !== 'failed') {
        throw new Error("'status' is not supported");
      }
      numberField(record, 'items_done');
      numberField(record, 'items_total');
      numberField(record, 'bytes_done');
      numberField(record, 'bytes_total');
      const mark = status === 'completed' ? '✓' : status === 'cancelled' ? '■' : '✕';
      const message = `${mark} ${phase} · ${status}`;
      return {
        timestampMs,
        level: status === 'failed' ? 'error' : 'info',
        scope: 'phase',
        message,
        searchText: message,
      };
    }
    case 'paused':
      return { timestampMs, level: 'info', scope: 'control', message: 'Paused', searchText: 'Paused' };
    case 'resumed': {
      const pausedMs = numberField(record, 'paused_ms');
      const message = `Resumed after ${humanDuration(pausedMs)}`;
      return { timestampMs, level: 'info', scope: 'control', message, searchText: message };
    }
    case 'summary': {
      const done = numberField(record, 'done');
      const skipped = numberField(record, 'skipped');
      const errors = numberField(record, 'errors');
      const bytesDone = numberField(record, 'bytes_done');
      const elapsedMs = numberField(record, 'elapsed_ms');
      numberField(record, 'paused_ms');
      const cancelled = booleanField(record, 'cancelled');
      const message = `■ Done: ${done} run, ${skipped} skipped, ${errors} errors · ${humanSize(bytesDone)} · ${(elapsedMs / 1000).toFixed(1)}s${cancelled ? ' · cancelled' : ''}`;
      return {
        timestampMs,
        level: errors ? 'error' : 'info',
        scope: 'final',
        message,
        searchText: message,
      };
    }
    default:
      throw new Error(`unsupported event kind '${kind}'`);
  }
}

export function parseLogArtifactLine(rawLine: string, view: LogArtifactView): LogRow {
  let decoded: unknown;
  try {
    decoded = JSON.parse(rawLine);
  } catch (error) {
    if (/^\s*[\[{]/.test(rawLine)) return parseFailure(rawLine, error);
    return {
      timestampMs: 0,
      level: 'info',
      scope: 'legacy',
      message: rawLine,
      searchText: rawLine,
    };
  }
  if (decoded === null || Array.isArray(decoded) || typeof decoded !== 'object') {
    return parseFailure(rawLine, 'the record must be a JSON object');
  }
  try {
    const record = decoded as Record<string, unknown>;
    return view === 'plan' ? parsePlanRow(record, rawLine) : parseRunEvent(record, rawLine);
  } catch (error) {
    return parseFailure(rawLine, error);
  }
}
