import assert from 'node:assert/strict';
import test from 'node:test';

import {
  flaggedDeletionSides,
  formatDeletionShare,
  summarizeApplyReview,
} from '#ui/features/apply-review/model/applyReviewTotals.ts';
import type { PlanDto, PlanOperation } from '#core/domain/compare/plan.ts';

function operation(
  side: PlanOperation['side'],
  action: PlanOperation['action'],
  path: string,
): PlanOperation {
  return { side, action, path, reason: 'fixture' };
}

function plan(
  operations: PlanOperation[],
  entries: { source: number; target: number },
): PlanDto {
  return {
    owner: {} as PlanDto['owner'],
    header: {
      source_entries: entries.source,
      target_entries: entries.target,
    } as PlanDto['header'],
    ops: operations,
    metas: operations.map(() => null),
    identical_count: 0,
    identical_bytes: 0,
    mtime_window_ms: 2000,
  };
}

function totalsOf(operations: PlanOperation[], entries: { source: number; target: number }) {
  const result = plan(operations, entries);
  const indices = operations.map((_, index) => index);
  return summarizeApplyReview(result, indices, operations.map(() => false), operations.map(() => true));
}

const repeat = (count: number, build: (index: number) => PlanOperation) =>
  Array.from({ length: count }, (_, index) => build(index));

/// `guard/ratio.rs`, reached from `guard/mod.rs`, judges each side separately against that side's
/// own snapshot entry count. A sync plan that empties most of the source measured its deletes
/// against the target's entries here and rendered "900% of target deleted" for a plan that deletes
/// nothing on the target — while the engine's own warning in the same sheet said
/// "source: plan deletes 900 of 1000 entries (90%)".
test('a deletion share is measured against the entries of the side it deletes from', () => {
  const totals = totalsOf(
    repeat(900, (index) => operation('source', 'delete', `s/${index}`)),
    { source: 1000, target: 100 },
  );

  const flagged = flaggedDeletionSides(totals, 0.5);
  assert.deepEqual(flagged.map((side) => side.side), ['source']);
  assert.deepEqual(flagged.map((side) => formatDeletionShare(side)), ['90% of source']);
});

/// The engine counts `Action::Delete` and keeps `Action::DeleteDir` in its own field
/// (`guard/stats.rs`), so a directory removal never enters the ratio. Folding it in marked a mirror
/// the engine had cleared.
test('a removed directory is not part of the deletion share', () => {
  const totals = totalsOf(
    [
      ...repeat(40, (index) => operation('target', 'delete', `t/${index}`)),
      ...repeat(20, (index) => operation('target', 'delete_dir', `t/dir${index}`)),
    ],
    { source: 100, target: 100 },
  );

  assert.equal(totals.deleteCount, 60, 'the row still counts every deletion it will perform');
  assert.deepEqual(
    flaggedDeletionSides(totals, 0.5),
    [],
    '40 of 100 entries is under the mark, exactly as the engine judges it',
  );
});

test('a side with no entries to judge against reports no share', () => {
  const totals = totalsOf(
    repeat(3, (index) => operation('target', 'delete', `t/${index}`)),
    { source: 0, target: 0 },
  );
  assert.deepEqual(flaggedDeletionSides(totals, 0.5), []);
  // Nothing to measure against is not a 0% or an Infinity; the caption is simply absent.
  assert.equal(formatDeletionShare({ side: 'target', deletes: 3, entries: 0 }), '');
});

/// A threshold outside (0, 1) is the job switching the mark off, which is how the engine reads the
/// same number.
test('the mark fires at the configured share and the job can switch it off', () => {
  const halfTheTarget = () => totalsOf(
    repeat(50, (index) => operation('target', 'delete', `t/${index}`)),
    { source: 100, target: 100 },
  );
  assert.deepEqual(
    flaggedDeletionSides(halfTheTarget(), 0.5).map((side) => side.side),
    ['target'],
    'exactly at the mark counts',
  );
  assert.deepEqual(flaggedDeletionSides(halfTheTarget(), 0.51), []);
  assert.deepEqual(flaggedDeletionSides(halfTheTarget(), 0), []);
  assert.deepEqual(flaggedDeletionSides(halfTheTarget(), 1), []);
});

/// The engine runs `check_delete_ratio` once per side and can push two independent warnings, so the
/// sheet states one caption per flagged side in that same order. A single caption had to pick one of
/// the two and drop the other, which hid half of what the engine had already said in the same sheet.
test('both flagged sides are named, in the order the engine checks them', () => {
  const totals = totalsOf(
    [
      ...repeat(90, (index) => operation('target', 'delete', `t/${index}`)),
      ...repeat(80, (index) => operation('source', 'delete', `s/${index}`)),
    ],
    { source: 100, target: 100 },
  );

  const flagged = flaggedDeletionSides(totals, 0.5);
  assert.deepEqual(flagged.map((side) => side.side), ['target', 'source']);
  assert.deepEqual(
    flagged.map((side) => formatDeletionShare(side)),
    ['90% of target', '80% of source'],
  );
});

/// Each side is judged on its own, so a plan that empties the target while barely touching the
/// source states one fact, not two, and names which side it is about.
test('only the side over the mark is named', () => {
  const totals = totalsOf(
    [
      ...repeat(90, (index) => operation('target', 'delete', `t/${index}`)),
      ...repeat(10, (index) => operation('source', 'delete', `s/${index}`)),
    ],
    { source: 100, target: 100 },
  );

  const flagged = flaggedDeletionSides(totals, 0.5);
  assert.deepEqual(flagged.map((side) => side.side), ['target']);
  assert.deepEqual(flagged.map((side) => formatDeletionShare(side)), ['90% of target']);
});

test('a plan under the mark on both sides is named on neither', () => {
  const totals = totalsOf(
    [
      ...repeat(10, (index) => operation('target', 'delete', `t/${index}`)),
      ...repeat(10, (index) => operation('source', 'delete', `s/${index}`)),
    ],
    { source: 100, target: 100 },
  );

  assert.equal(totals.deleteCount, 20, 'the row still counts every deletion it will perform');
  assert.deepEqual(flaggedDeletionSides(totals, 0.5), []);
});
