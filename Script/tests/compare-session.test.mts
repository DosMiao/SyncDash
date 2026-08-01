import assert from 'node:assert/strict';
import test from 'node:test';

import {
  activeSession,
  invalidateJobSession,
  reconcileRefreshedJobSession,
  reconcileSavedJobSession,
  successfulSession,
  targetForSelection,
} from '../../typescript/ui/state/compare-session.ts';
import type { PlanDto } from '../../typescript/core/plan.ts';
import type { CompareOwner } from '../../typescript/core/types/generated/CompareOwner.ts';
import type { JobDto } from '../../typescript/core/types/generated/JobDto.ts';
import { selectedRows } from '../../typescript/core/plan.ts';

function owner(jobName: string, targetIndex: number, revision: string, compareId = 1): CompareOwner {
  return { compare_id: compareId, job_name: jobName, target_index: targetIndex, config_revision: revision };
}

function plan(o: CompareOwner): PlanDto {
  return {
    owner: o,
    header: {} as PlanDto['header'],
    ops: [],
    metas: [],
    equal_count: 0,
    equal_bytes: 0,
  };
}

function job(name: string, revision: string, targets = ['target']): JobDto {
  return { name, config_revision: revision, targets } as JobDto;
}

test('A to B without comparing and back to A restores the one retained review session', () => {
  const a = job('A', 'rev-a', ['a0', 'a1']);
  const b = job('B', 'rev-b');
  const slot = successfulSession(plan(owner('A', 1, 'rev-a')), [true, false], [false, true]);

  assert.equal(activeSession(slot, a, 1), slot);
  assert.equal(activeSession(slot, b, 0), null);
  assert.equal(targetForSelection(slot, a), 1);

  const restored = activeSession(slot, a, targetForSelection(slot, a));
  assert.equal(restored, slot);
  assert.deepEqual(restored?.checked, [true, false]);
  assert.deepEqual(restored?.flipped, [false, true]);
});

test('target and config revision are both part of result ownership', () => {
  const slot = successfulSession(plan(owner('A', 1, 'rev-a')), [], []);

  assert.equal(activeSession(slot, job('A', 'rev-a', ['a0', 'a1']), 0), null);
  assert.equal(activeSession(slot, job('A', 'rev-new', ['a0', 'a1']), 1), null);
  assert.equal(targetForSelection(slot, job('A', 'rev-new', ['a0', 'a1'])), 0);
  assert.equal(targetForSelection(slot, job('A', 'rev-a', ['only-target'])), 0);
});

test('a successful result replaces the bounded slot and mutation invalidates only its job', () => {
  const first = successfulSession(plan(owner('A', 0, 'rev-a', 1)), [true], [false]);
  const second = successfulSession(plan(owner('B', 0, 'rev-b', 2)), [false], [true]);

  assert.notEqual(second, first);
  assert.equal(invalidateJobSession(second, 'A'), second);
  assert.equal(invalidateJobSession(second, 'B'), null);
});

test('a no-op save retains the session while an effective revision change retires it', () => {
  const slot = successfulSession(plan(owner('A', 0, 'rev-a')), [true], [false]);

  assert.equal(reconcileSavedJobSession(slot, 'A', 'rev-a'), slot);
  assert.equal(reconcileSavedJobSession(slot, 'B', 'rev-new'), slot);
  assert.equal(reconcileSavedJobSession(slot, 'A', 'rev-new'), null);
});

test('failed compare refresh retains unchanged ownership, hides changed ownership, and clears a missing job', () => {
  const unchanged = job('A', 'rev-a', ['a0', 'a1']);
  const changed = job('A', 'rev-new', ['a0']);
  const slot = successfulSession(plan(owner('A', 1, 'rev-a')), [true], [false]);

  const retained = reconcileRefreshedJobSession(slot, 'A', unchanged);
  assert.equal(retained, slot);
  assert.equal(activeSession(retained, unchanged, 1), slot);

  const hidden = reconcileRefreshedJobSession(slot, 'A', changed);
  assert.equal(hidden, slot);
  assert.equal(activeSession(hidden, changed, 0), null);
  assert.equal(targetForSelection(hidden, changed), 0);

  assert.equal(reconcileRefreshedJobSession(slot, 'A', null), null);
  assert.equal(reconcileRefreshedJobSession(slot, 'B', null), slot);
});

test('the apply payload contains only authenticated row decisions', () => {
  assert.deepEqual(selectedRows([2, 0], [false, false, true]), [
    { index: 2, flipped: true },
    { index: 0, flipped: false },
  ]);
});
