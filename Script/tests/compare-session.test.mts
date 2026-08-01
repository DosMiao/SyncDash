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
  rebindSessionOwner,
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
import type { JobSaveDto } from '../../typescript/core/types/generated/JobSaveDto.ts';
import { selectedRows } from '../../typescript/core/plan.ts';

function owner(jobName: string, targetIndex: number, revision: string, compareId = 1, jobId = `id-${jobName}`): CompareOwner {
  return { compare_id: compareId, job_id: jobId, job_name: jobName, target_index: targetIndex, config_revision: revision };
}

function plan(o: CompareOwner): PlanDto {
  return {
    owner: o,
    header: {} as PlanDto['header'],
    ops: [],
    metas: [],
    identical_count: 0,
    identical_bytes: 0,
  };
}

function job(name: string, revision: string, targets = ['target'], jobId = `id-${name}`): JobDto {
  return { job_id: jobId, name, config_revision: revision, targets } as JobDto;
}

function mutation(
  effect: JobSaveDto['effect'],
  name: string,
  revision: string,
  jobId = `id-${name}`,
  previousName: string | null = null,
): JobSaveDto {
  return { effect, job_id: jobId, name, config_revision: revision, previous_name: previousName };
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

test('stable identity, not a reused display name, owns a retained result', () => {
  const repository = retain(EMPTY_COMPARE_REPOSITORY, 'A', 0, 'rev-a', 1);

  assert.ok(activeSession(repository, job('A', 'rev-a'), 0));
  assert.equal(activeSession(repository, job('A', 'rev-a', ['target'], 'replacement-id'), 0), null);
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

  const original = { jobId: 'id-A', name: 'A', configRevision: 'rev-a' };
  const noOp = reconcileSavedJobSession(repository, mutation('no_op', 'A', 'rev-a', 'id-A'), original);
  assert.equal(noOp, repository);

  const changed = reconcileSavedJobSession(
    repository,
    mutation('updated', 'A', 'rev-next', 'id-A'),
    original,
  );
  assert.equal(activeSession(changed, job('A', 'rev-a'), 0), null);
  assert.ok(activeSession(changed, job('A', 'rev-new'), 0));
  assert.ok(activeSession(changed, job('B', 'rev-a'), 0));

  const renamed = reconcileSavedJobSession(
    repository,
    mutation('renamed', 'Renamed', 'rev-a', 'id-A', 'A'),
    original,
  );
  assert.equal(activeSession(renamed, job('Renamed', 'rev-a', ['target'], 'id-A'), 0)?.plan.owner.compare_id, 1);
  assert.equal(renamed.sessions.find((session) => session.plan.owner.job_id === 'id-A')?.plan.owner.job_name, 'Renamed');
  assert.ok(activeSession(renamed, job('B', 'rev-a'), 0));

  assert.equal(invalidateJobRevision(repository, 'missing', 'rev-a'), repository);
  assert.equal(invalidateJobSession(repository, 'id-B').sessions.length, 2);
});

test('authoritative refresh drops only obsolete revisions or all results for a missing job', () => {
  let repository = retain(EMPTY_COMPARE_REPOSITORY, 'A', 0, 'rev-a', 1);
  repository = retain(repository, 'A', 0, 'rev-new', 2);
  repository = retain(repository, 'B', 0, 'rev-b', 3);

  const original = { jobId: 'id-A', name: 'A', configRevision: 'rev-a' };
  const refreshed = reconcileRefreshedJobSession(repository, original, job('Renamed', 'rev-new', ['target'], 'id-A'));
  assert.equal(activeSession(refreshed, job('A', 'rev-a'), 0), null);
  assert.ok(activeSession(refreshed, job('Renamed', 'rev-new', ['target'], 'id-A'), 0));
  assert.ok(activeSession(refreshed, job('B', 'rev-b'), 0));

  const missing = reconcileRefreshedJobSession(repository, original, null);
  assert.equal(missing.sessions.length, 1);
  assert.ok(activeSession(missing, job('B', 'rev-b'), 0));
});

test('touch rebinds a renamed owner without changing the compare result identity', () => {
  const repository = retain(EMPTY_COMPARE_REPOSITORY, 'A', 0, 'rev-a', 7);
  const previous = owner('A', 0, 'rev-a', 7);
  const current = { ...previous, job_name: 'Renamed' };
  const rebound = rebindSessionOwner(repository, previous, current);

  assert.equal(rebound.sessions[0].plan.owner.job_name, 'Renamed');
  assert.equal(rebound.sessions[0].plan.owner.compare_id, 7);
  assert.equal(rebound.sessions[0].plan.owner.job_id, 'id-A');
});

test('the apply payload contains only authenticated row decisions', () => {
  assert.deepEqual(selectedRows([2, 0], [false, false, true]), [
    { index: 2, flipped: true },
    { index: 0, flipped: false },
  ]);
});
