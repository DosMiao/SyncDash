import type { RunSummaryEvent } from '#core/domain/runs/runEvents.ts';
import type { Phase } from '#core/types/generated/Phase.ts';
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  NO_COMPARE_RUN_FAULTS,
  recordCompareFault,
  reduceCompareStages,
} from '#core/domain/compare/compareProgress.ts';
import type { CompareProgressEvent } from '#core/domain/compare/compareProgress.ts';
import { mergeRunEventReplay } from '#core/domain/runs/runEvents.ts';
import {
  completionPercent,
  endStage,
  newRunState,
  startStage,
} from '#core/application/progress/runstate.ts';
import type { ProgressStage } from '#core/application/progress/runstate.ts';

/// The envelope is a discriminated union now, so a fixture cannot claim to be every variant at
/// once. Building it loosely and asserting the shape at the boundary keeps these cases readable
/// without weakening the type the production code sees.
const event = (
  kind: CompareProgressEvent['kind'],
  phase: Phase,
  extra: Record<string, unknown> = {},
): CompareProgressEvent => ({
  sequence: 1, kind, phase, run_id: 1, purpose: 'compare', ts_ms: 1, ...extra,
} as unknown as CompareProgressEvent);

const errorEvent = (extra: Record<string, unknown>): CompareProgressEvent => ({
  sequence: 1, kind: 'error', phase: 'scan-source', run_id: 1, purpose: 'compare', ts_ms: 1,
  path: '', action: 'hash', side: 'source', message: '', ...extra,
} as unknown as CompareProgressEvent);

/// A walk error event restates what the finished plan header carries in full, so counting it here
/// too would show one denied subtree as two problems.
test('walk errors stay out of the run faults the header already reports', () => {
  const faults = recordCompareFault(NO_COMPARE_RUN_FAULTS, errorEvent({
    action: 'walk', message: '2 subtree(s) could not be read',
  }));
  assert.deepEqual(faults, NO_COMPARE_RUN_FAULTS);
});

/// A per-file read failure appears in no header list, and the path it names is the only place the
/// user can learn which file it was.
test('a per-file read failure is retained with its path', () => {
  const faults = recordCompareFault(NO_COMPARE_RUN_FAULTS, errorEvent({
    path: 'a/b.dat', message: 'file changed while content evidence was being read',
  }));
  assert.equal(faults.total, 1);
  assert.equal(faults.retained[0].path, 'a/b.dat');
});

test('an identical repeat still counts but is listed once', () => {
  const one = recordCompareFault(NO_COMPARE_RUN_FAULTS, errorEvent({ path: 'a', message: 'x' }));
  const two = recordCompareFault(one, errorEvent({ path: 'a', message: 'x' }));
  assert.equal(two.total, 2);
  assert.equal(two.retained.length, 1);
});

/// The cap is what makes the count and the list diverge, and the reader is told rather than shown
/// a list that silently stopped.
test('past the retention cap the total keeps counting', () => {
  let faults = NO_COMPARE_RUN_FAULTS;
  for (let index = 0; index < 60; index += 1) {
    faults = recordCompareFault(faults, errorEvent({ path: `f${index}`, message: 'unreadable' }));
  }
  assert.equal(faults.total, 60);
  assert.equal(faults.retained.length, 50);
});

test('parallel scan start does not complete the other scan', () => {
  let stages = reduceCompareStages([], event('phase_start', 'scan-target'));
  stages = reduceCompareStages(stages, event('progress', 'scan-target', {
    items_done: 40, items_total: 100, bytes_done: 400, bytes_total: 1000,
  }));
  stages = reduceCompareStages(stages, event('phase_start', 'scan-source'));

  const target = stages.find((stage) => stage.phase === 'scan-target')!;
  assert.equal(target.active, true);
  assert.equal(target.done, false);
  assert.equal(target.itemsDone, 40);
});

test('only that phase explicit end changes its terminal state', () => {
  let stages = reduceCompareStages([], event('phase_start', 'scan-target'));
  stages = reduceCompareStages(stages, event('phase_start', 'scan-source'));
  stages = reduceCompareStages(stages, event('phase_end', 'scan-target', {
    status: 'completed', items_done: 100, items_total: 100,
  }));

  assert.equal(stages.find((stage) => stage.phase === 'scan-target')!.done, true);
  assert.equal(stages.find((stage) => stage.phase === 'scan-source')!.active, true);

  stages = reduceCompareStages(stages, event('phase_end', 'scan-source', { status: 'failed' }));
  const source = stages.find((stage) => stage.phase === 'scan-source')!;
  assert.equal(source.active, false);
  assert.equal(source.done, false);
  assert.equal(source.failed, true);
});

test('delayed worker snapshots cannot regress a compare row', () => {
  let stages = reduceCompareStages([], event('phase_start', 'scan-source'));
  stages = reduceCompareStages(stages, event('progress', 'scan-source', {
    items_done: 10, items_total: 100, bytes_done: 500, bytes_total: 1000,
  }));
  stages = reduceCompareStages(stages, event('progress', 'scan-source', {
    items_done: 9, items_total: 100, bytes_done: 400, bytes_total: 1000,
  }));

  assert.equal(stages[0].itemsDone, 10);
  assert.equal(stages[0].bytesDone, 500);
});

/// A terminal summary, with every counter the wire type requires.
const summary = (over: Partial<RunSummaryEvent> = {}): RunSummaryEvent => ({
  sequence: 1, kind: 'summary', run_id: 1, ts_ms: Date.now(), purpose: 'apply',
  done: 0, skipped: 0, errors: 0, bytes_done: 0, elapsed_ms: 0, paused_ms: 0, cancelled: false,
  ...over,
});

test('live progress reserves 100 for a successful Summary', () => {
  const run = newRunState(1, Date.now());
  run.totals = { items: 1, bytes: 1000 };
  run.completed = { items: 0, bytes: 1000 };
  assert.equal(completionPercent(run), 99);

  run.running = false;
  run.summary = summary({ done: 1 });
  assert.equal(completionPercent(run), 100);

  const empty = newRunState(2, Date.now());
  empty.running = false;
  empty.summary = summary({ run_id: 2 });
  assert.equal(completionPercent(empty), 100);
});

test('a repeated phase cannot erase an earlier failure', () => {
  const row: ProgressStage = { phase: 'apply', detail: '', active: true, done: false };
  endStage(row, 'failed');
  startStage(row);
  endStage(row, 'completed');

  assert.equal(row.active, false);
  assert.equal(row.done, false);
  assert.equal(row.failed, true);
});

test('reconnect merges the snapshot with events received during the request exactly once', () => {
  const one = event('phase_start', 'scan-source', { sequence: 1 });
  const two = event('progress', 'scan-source', { sequence: 2, items_done: 1 });
  const three = event('progress', 'scan-source', { sequence: 3, items_done: 2 });

  const merged = mergeRunEventReplay([one, two], [two, three]);

  assert.deepEqual(merged.map((item) => item.sequence), [1, 2, 3]);
});
