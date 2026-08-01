import assert from 'node:assert/strict';
import test from 'node:test';

import {
  activeSession,
  COMPARE_SESSION_CAPACITY,
  EMPTY_COMPARE_REPOSITORY,
  invalidateJobRevision,
  invalidateJobSession,
  invalidateSession,
  reconcileRefreshedJobSession,
  reconcileSavedJobSession,
  retainSuccessfulSession,
  successfulSession,
  targetForSelection,
  touchSession,
  updateSession,
} from '../../typescript/ui/state/compare-session.ts';
import type { CompareRepository } from '../../typescript/ui/state/compare-session.ts';
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

function retain(
  repository: CompareRepository,
  jobName: string,
  targetIndex: number,
  revision: string,
  compareId: number,
): CompareRepository {
  return retainSuccessfulSession(
    repository,
    successfulSession(plan(owner(jobName, targetIndex, revision, compareId)), [true], [false]),
  );
}

test('switching among jobs and targets restores every retained review session', () => {
  const a = job('A', 'rev-a', ['a0', 'a1']);
  const b = job('B', 'rev-b');
  let repository = retain(EMPTY_COMPARE_REPOSITORY, 'A', 1, 'rev-a', 1);
  repository = retain(repository, 'B', 0, 'rev-b', 2);

  assert.equal(activeSession(repository, a, 1)?.plan.owner.compare_id, 1);
  assert.equal(activeSession(repository, b, 0)?.plan.owner.compare_id, 2);
  assert.equal(targetForSelection(repository, a), 1);
  assert.equal(targetForSelection(repository, b), 0);
});

test('target and config revision are both part of result ownership', () => {
  const repository = retain(EMPTY_COMPARE_REPOSITORY, 'A', 1, 'rev-a', 1);

  assert.equal(activeSession(repository, job('A', 'rev-a', ['a0', 'a1']), 0), null);
  assert.equal(activeSession(repository, job('A', 'rev-new', ['a0', 'a1']), 1), null);
  assert.equal(targetForSelection(repository, job('A', 'rev-new', ['a0', 'a1'])), 0);
  assert.equal(targetForSelection(repository, job('A', 'rev-a', ['only-target'])), 0);
});

test('the repository is LRU bounded and a failed compare has no state transition to evict success', () => {
  let repository = EMPTY_COMPARE_REPOSITORY;
  for (let i = 0; i < COMPARE_SESSION_CAPACITY; i++) {
    repository = retain(repository, `job-${i}`, 0, `rev-${i}`, i);
  }
  repository = touchSession(repository, job('job-0', 'rev-0'), 0);

  const unchangedAfterFailure = repository;
  assert.equal(activeSession(unchangedAfterFailure, job('job-0', 'rev-0'), 0)?.plan.owner.compare_id, 0);

  repository = retain(repository, 'new-job', 0, 'new-rev', 100);
  assert.equal(repository.sessions.length, COMPARE_SESSION_CAPACITY);
  assert.ok(activeSession(repository, job('job-0', 'rev-0'), 0));
  assert.equal(activeSession(repository, job('job-1', 'rev-1'), 0), null);
});

test('a newer successful compare replaces only the same job-target-revision key', () => {
  let repository = retain(EMPTY_COMPARE_REPOSITORY, 'A', 0, 'rev-a', 1);
  repository = retain(repository, 'A', 1, 'rev-a', 2);
  repository = retain(repository, 'A', 0, 'rev-a', 3);

  assert.equal(repository.sessions.length, 2);
  assert.equal(activeSession(repository, job('A', 'rev-a', ['a0', 'a1']), 0)?.plan.owner.compare_id, 3);
  assert.equal(activeSession(repository, job('A', 'rev-a', ['a0', 'a1']), 1)?.plan.owner.compare_id, 2);
  const withoutTargetZero = invalidateSession(repository, owner('A', 0, 'rev-a', 999));
  assert.equal(activeSession(withoutTargetZero, job('A', 'rev-a', ['a0', 'a1']), 0), null);
  assert.ok(activeSession(withoutTargetZero, job('A', 'rev-a', ['a0', 'a1']), 1));
});

test('review changes update only the selected retained result', () => {
  let repository = retain(EMPTY_COMPARE_REPOSITORY, 'A', 0, 'rev-a', 1);
  repository = retain(repository, 'B', 0, 'rev-b', 2);
  repository = updateSession(repository, job('A', 'rev-a'), 0, (session) => ({
    ...session,
    checked: [false],
    flipped: [true],
  }));

  assert.deepEqual(activeSession(repository, job('A', 'rev-a'), 0)?.checked, [false]);
  assert.deepEqual(activeSession(repository, job('A', 'rev-a'), 0)?.flipped, [true]);
  assert.deepEqual(activeSession(repository, job('B', 'rev-b'), 0)?.checked, [true]);
});

test('effective mutation invalidates only the affected job revision while a no-op save retains it', () => {
  let repository = retain(EMPTY_COMPARE_REPOSITORY, 'A', 0, 'rev-a', 1);
  repository = retain(repository, 'A', 0, 'rev-new', 2);
  repository = retain(repository, 'B', 0, 'rev-a', 3);

  const noOp = reconcileSavedJobSession(repository, 'A', 'rev-a', 'A', 'rev-a');
  assert.equal(noOp, repository);

  const changed = reconcileSavedJobSession(repository, 'A', 'rev-a', 'A', 'rev-next');
  assert.equal(activeSession(changed, job('A', 'rev-a'), 0), null);
  assert.ok(activeSession(changed, job('A', 'rev-new'), 0));
  assert.ok(activeSession(changed, job('B', 'rev-a'), 0));

  const renamed = reconcileSavedJobSession(repository, 'A', 'rev-a', 'Renamed', 'rev-a');
  assert.equal(renamed.sessions.filter((session) => session.plan.owner.job_name === 'A').length, 0);
  assert.ok(activeSession(renamed, job('B', 'rev-a'), 0));

  assert.equal(invalidateJobRevision(repository, 'missing', 'rev-a'), repository);
  assert.equal(invalidateJobSession(repository, 'B').sessions.length, 2);
});

test('authoritative refresh drops only obsolete revisions or all results for a missing job', () => {
  let repository = retain(EMPTY_COMPARE_REPOSITORY, 'A', 0, 'rev-a', 1);
  repository = retain(repository, 'A', 0, 'rev-new', 2);
  repository = retain(repository, 'B', 0, 'rev-b', 3);

  const refreshed = reconcileRefreshedJobSession(repository, 'A', job('A', 'rev-new'));
  assert.equal(activeSession(refreshed, job('A', 'rev-a'), 0), null);
  assert.ok(activeSession(refreshed, job('A', 'rev-new'), 0));
  assert.ok(activeSession(refreshed, job('B', 'rev-b'), 0));

  const missing = reconcileRefreshedJobSession(repository, 'A', null);
  assert.equal(missing.sessions.length, 1);
  assert.ok(activeSession(missing, job('B', 'rev-b'), 0));
});

test('the apply payload contains only authenticated row decisions', () => {
  assert.deepEqual(selectedRows([2, 0], [false, false, true]), [
    { index: 2, flipped: true },
    { index: 0, flipped: false },
  ]);
});
