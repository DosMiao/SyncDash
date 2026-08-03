import assert from 'node:assert/strict';
import test from 'node:test';

import {
  flaggedDeletionSide,
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

  const flagged = flaggedDeletionSide(totals, 0.5);
  assert.equal(flagged?.side, 'source');
  assert.equal(formatDeletionShare(flagged), '90% of source');
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
  assert.equal(
    flaggedDeletionSide(totals, 0.5),
    null,
    '40 of 100 entries is under the mark, exactly as the engine judges it',
  );
});

test('a side with no entries to judge against reports no share', () => {
  const totals = totalsOf(
    repeat(3, (index) => operation('target', 'delete', `t/${index}`)),
    { source: 0, target: 0 },
  );
  assert.equal(flaggedDeletionSide(totals, 0.5), null);
  assert.equal(formatDeletionShare(null), '');
});

/// A threshold outside (0, 1) is the job switching the mark off, which is how the engine reads the
/// same number.
test('the mark fires at the configured share and the job can switch it off', () => {
  const halfTheTarget = () => totalsOf(
    repeat(50, (index) => operation('target', 'delete', `t/${index}`)),
    { source: 100, target: 100 },
  );
  assert.equal(flaggedDeletionSide(halfTheTarget(), 0.5)?.side, 'target', 'exactly at the mark counts');
  assert.equal(flaggedDeletionSide(halfTheTarget(), 0.51), null);
  assert.equal(flaggedDeletionSide(halfTheTarget(), 0), null);
  assert.equal(flaggedDeletionSide(halfTheTarget(), 1), null);
});
