import assert from 'node:assert/strict';
import test from 'node:test';

import { reduceCompareStages } from '../../typescript/core/compareProgress.ts';
import type { CompareProgressEvent } from '../../typescript/core/compareProgress.ts';
import { mergeRunEventReplay } from '../../typescript/core/runEvents.ts';
import { endStage, newRunState, percent, startStage } from '../../typescript/progress/runstate.ts';
import type { StageRow } from '../../typescript/progress/runstate.ts';

const event = (
  kind: CompareProgressEvent['kind'],
  phase: CompareProgressEvent['phase'],
  extra: Partial<CompareProgressEvent> = {},
): CompareProgressEvent => ({
  sequence: 1, kind, phase, run_id: 1, purpose: 'compare', ts_ms: 1, ...extra,
});

test('parallel scan start does not complete the other scan', () => {
  let stages = reduceCompareStages([], event('phase_start', 'scan-target'));
  stages = reduceCompareStages(stages, event('progress', 'scan-target', {
    items_done: 40, items_total: 100, bytes_done: 400, bytes_total: 1000,
  }));
  stages = reduceCompareStages(stages, event('phase_start', 'scan-source'));

  const target = stages.find((s) => s.phase === 'scan-target')!;
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

  assert.equal(stages.find((s) => s.phase === 'scan-target')!.done, true);
  assert.equal(stages.find((s) => s.phase === 'scan-source')!.active, true);

  stages = reduceCompareStages(stages, event('phase_end', 'scan-source', { status: 'failed' }));
  const source = stages.find((s) => s.phase === 'scan-source')!;
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

test('live progress reserves 100 for a successful Summary', () => {
  const run = newRunState(1, Date.now());
  run.totals = { items: 1, bytes: 1000 };
  run.dones = { items: 0, bytes: 1000 };
  assert.equal(percent(run), 99);

  run.running = false;
  run.summary = {
    kind: 'summary', run_id: 1, ts_ms: Date.now(), purpose: 'apply',
    done: 1, skipped: 0, errors: 0, cancelled: false,
  };
  assert.equal(percent(run), 100);

  const empty = newRunState(2, Date.now());
  empty.running = false;
  empty.summary = {
    kind: 'summary', run_id: 2, ts_ms: Date.now(), purpose: 'apply',
    done: 0, skipped: 0, errors: 0, cancelled: false,
  };
  assert.equal(percent(empty), 100);
});

test('a repeated phase cannot erase an earlier failure', () => {
  const row: StageRow = { phase: 'apply', detail: '', active: true, done: false };
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
