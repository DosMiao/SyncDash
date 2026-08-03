import assert from 'node:assert/strict';
import test from 'node:test';

import {
  formatPlanShare,
  planShare,
  planShareIsHigh,
} from '#ui/features/apply-review/model/applyReviewTotals.ts';

test('a share is only meaningful against entries the target actually holds', () => {
  assert.equal(planShare(60, 100), 0.6);
  // Nothing to measure, and nothing measured: neither may become a fabricated 0%.
  assert.equal(planShare(0, 100), null);
  assert.equal(planShare(60, 0), null);
  assert.equal(formatPlanShare(0, 100), '');
  assert.equal(formatPlanShare(60, 100), '60% of target');
});

test('the highlight fires at the configured share and the job can switch it off', () => {
  assert.equal(planShareIsHigh(50, 100, 0.5), true, 'exactly at the mark counts');
  assert.equal(planShareIsHigh(49, 100, 0.5), false);
  // A threshold outside (0, 1) is the job turning the highlight off, matching how the engine reads
  // the same number — not an always-on or always-off highlight.
  assert.equal(planShareIsHigh(100, 100, 0), false);
  assert.equal(planShareIsHigh(100, 100, 1), false);
});

/// A first mirror onto a near-empty disk is thousands of copies against a couple of entries. Only
/// operations that displace something belong in a share, so the caller passes updates rather than
/// copies — passing the write total reported "792350% of target" and colored a safe run red.
test('a first mirror onto a near-empty target reports no displacement', () => {
  const copies = 15_847;
  const updates = 0;
  const targetEntries = 2;

  assert.equal(planShareIsHigh(updates, targetEntries, 0.5), false);
  assert.equal(formatPlanShare(updates, targetEntries), '');
  // The arithmetic that produced the bad number is still arithmetic; it just is not what we ask.
  assert.equal(Math.round((planShare(copies, targetEntries) ?? 0) * 100), 792_350);
});
