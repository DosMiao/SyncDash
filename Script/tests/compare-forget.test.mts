import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import test from 'node:test';

import {
  compareResultKey,
  compareScopeFromIdentity,
  compareScopeKey,
  emptyCompareWorkspaceRepository,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type {
  CompareResultKey,
  CompareScopeWorkspace,
  CompareWorkspaceRepository,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import {
  deriveCompareForgetAvailability,
  resolveCompareResultForget,
} from '#core/application/compare-workspace/compareWorkspaceForget.ts';
import { reduceCompareWorkspaces } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import type { PlanDto, PlanOperation } from '#core/domain/compare/plan.ts';
import type { CompareIdentity } from '#core/types/generated/CompareIdentity.ts';
import type { CompareOwner } from '#core/types/generated/CompareOwner.ts';
import type { CompareScopeExecutionStatusDto } from '#core/types/generated/CompareScopeExecutionStatusDto.ts';
import type { CompareWorkspaceSnapshotDto } from '#core/types/generated/CompareWorkspaceSnapshotDto.ts';

function identity(jobId: string, targetIndex: number, configRevision: string, compareRunId: number): CompareIdentity {
  const resultId = createHash('sha256')
    .update(JSON.stringify([jobId, targetIndex, configRevision, compareRunId]))
    .digest('hex')
    .slice(0, 32);
  return {
    result_id: resultId,
    job_id: jobId,
    target_index: targetIndex,
    config_revision: configRevision,
    compare_run_id: compareRunId,
  };
}

function owner(jobId: string, targetIndex: number, configRevision: string, compareRunId: number): CompareOwner {
  return { identity: identity(jobId, targetIndex, configRevision, compareRunId), job_name: jobId };
}

function operation(action: PlanOperation['action'], path: string): PlanOperation {
  return {
    side: 'target',
    action,
    path,
    size: 128,
    mtime_ms: 1_700_000_000_000,
    reason: `${action} fixture`,
  };
}

function plan(resultOwner: CompareOwner): PlanDto {
  const operations = [operation('copy', 'docs/a.txt'), operation('delete', 'archive/old.txt')];
  return {
    owner: resultOwner,
    header: {
      schema: 1,
      kind: 'compare',
      mode: 'mirror',
      generated_at_ms: 1_700_000_000_000,
      source_root: '/source',
      source_host: 'source',
      target_root: '/target',
      target_host: 'target',
      op_count: operations.length,
      conflict_count: 0,
      source_entries: 2,
      target_entries: 2,
      source_excluded: 0,
      target_excluded: 0,
      source_walk_errors: 0,
      target_walk_errors: 0,
      source_walk_err_samples: [],
      target_walk_err_samples: [],
      source_icloud_stubs: 0,
      target_icloud_stubs: 0,
      source_icloud_stub_samples: [],
      target_icloud_stub_samples: [],
      source_unread_paths: [],
      target_unread_paths: [],
      source_unread_entries: 0,
      target_unread_entries: 0,
    },
    ops: operations,
    metas: operations.map(() => null),
    identical_count: 1,
    identical_bytes: 64,
    mtime_window_ms: 2000,
  };
}

function fresh(
  resultPlan: PlanDto,
  verificationEpoch: number,
): Extract<CompareScopeExecutionStatusDto, { status: 'fresh' }> {
  return {
    status: 'fresh',
    scope: compareScopeFromIdentity(resultPlan.owner.identity),
    attempt: {
      verification_epoch: verificationEpoch,
      compare_run_id: resultPlan.owner.identity.compare_run_id,
    },
    owner: resultPlan.owner,
  };
}

function snapshot(resultPlan: PlanDto, verificationEpoch: number): CompareWorkspaceSnapshotDto {
  return { plan: resultPlan, execution_status: fresh(resultPlan, verificationEpoch) };
}

function dispatch(
  repository: CompareWorkspaceRepository,
  ...actions: CompareWorkspaceAction[]
): CompareWorkspaceRepository {
  return actions.reduce(reduceCompareWorkspaces, repository);
}

function publish(
  repository: CompareWorkspaceRepository,
  resultPlan: PlanDto,
  verificationEpoch: number,
): CompareWorkspaceRepository {
  return dispatch(repository, {
    type: 'manual_compare_published',
    snapshot: snapshot(resultPlan, verificationEpoch),
  });
}

function publishAutoScan(
  repository: CompareWorkspaceRepository,
  resultPlan: PlanDto,
  verificationEpoch: number,
  generation = 1,
  ticketId = 1,
): CompareWorkspaceRepository {
  return dispatch(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(resultPlan, verificationEpoch),
    generation,
    ticketId,
  });
}

function onlyScope(repository: CompareWorkspaceRepository): CompareScopeWorkspace {
  assert.equal(repository.scopes.length, 1);
  return repository.scopes[0];
}

const IDLE_GATE = { runInFlight: false, reviewPending: false };

const planA = plan(owner('job-a', 0, 'rev-a', 11));
const planB = plan(owner('job-a', 0, 'rev-a', 12));
const keyA = compareResultKey(planA.owner.identity);
const keyB = compareResultKey(planB.owner.identity);
const scopeKeyA = compareScopeKey(compareScopeFromIdentity(planA.owner.identity));

test('forgetting the active result drops it from the scope and closes its execution authority', () => {
  let repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  repository = dispatch(repository, {
    type: 'row_inclusion_replaced',
    resultKey: keyA,
    rowIncluded: [false, true],
  });
  assert.equal(onlyScope(repository).active?.key, keyA);

  repository = dispatch(repository, { type: 'result_forgotten', resultKey: keyA });

  const scope = onlyScope(repository);
  assert.equal(scope.active, null);
  assert.equal(scope.candidate, null);
  assert.equal(scope.execution, null, 'execution authority owned by forgotten evidence must not survive it');
});

test('forgetting the active result promotes the staged AutoScan candidate rather than stranding it', () => {
  let repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  repository = publishAutoScan(repository, planB, 2);
  assert.equal(onlyScope(repository).candidate?.workspace.key, keyB);

  repository = dispatch(repository, { type: 'result_forgotten', resultKey: keyA });

  const scope = onlyScope(repository);
  assert.equal(scope.active?.key, keyB, 'the surviving result must remain reachable without a reload');
  assert.equal(scope.candidate, null);
  assert.equal(scope.execution?.status, 'fresh', 'the candidate owned execution authority and keeps it');
});

test('forgetting a staged candidate leaves the active review workspace and its cursors clean', () => {
  let repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  repository = publishAutoScan(repository, planB, 2, 3, 7);
  assert.equal(onlyScope(repository).latestAutoScanPublication?.resultKey, keyB);

  repository = dispatch(repository, { type: 'result_forgotten', resultKey: keyB });

  const scope = onlyScope(repository);
  assert.equal(scope.active?.key, keyA);
  assert.equal(scope.candidate, null);
  assert.equal(
    scope.latestAutoScanPublication,
    null,
    'no cursor may keep pointing at evidence that no longer exists',
  );
});

test('forgetting a dismissed AutoScan candidate clears the dismissal it was recorded under', () => {
  let repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  repository = publishAutoScan(repository, planB, 2);
  repository = dispatch(repository, {
    type: 'candidate_discarded',
    scopeKey: scopeKeyA,
    expectedResultKey: keyB,
  });
  assert.equal(onlyScope(repository).dismissedCandidateKey, keyB);

  repository = dispatch(repository, { type: 'result_forgotten', resultKey: keyB });

  assert.equal(onlyScope(repository).dismissedCandidateKey, null);
});

test('forgetting an unknown result key changes nothing', () => {
  const repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  const unknown = 'f'.repeat(32) as CompareResultKey;

  assert.equal(
    dispatch(repository, { type: 'result_forgotten', resultKey: unknown }),
    repository,
  );
});

test('forget is refused while the result is executing or otherwise in use', () => {
  const repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  const scope = onlyScope(repository);
  const workspace = scope.active!;

  const running = deriveCompareForgetAvailability({
    scope,
    workspace,
    runInFlight: true,
    reviewPending: false,
  });
  assert.equal(running.available, false);
  assert.equal(running.available === false && running.reason, 'run_in_progress');

  const reviewing = deriveCompareForgetAvailability({
    scope,
    workspace,
    runInFlight: false,
    reviewPending: true,
  });
  assert.equal(reviewing.available, false);
  assert.equal(reviewing.available === false && reviewing.reason, 'review_open');

  const comparing = deriveCompareForgetAvailability({
    scope: {
      ...scope,
      activity: { status: 'comparing', requestId: 4, origin: { kind: 'interactive' } },
    },
    workspace,
    ...IDLE_GATE,
  });
  assert.equal(comparing.available, false);
  assert.equal(comparing.available === false && comparing.reason, 'compare_in_progress');

  const restoring = deriveCompareForgetAvailability({
    scope: { ...scope, restoration: { status: 'loading', requestId: 4 } },
    workspace,
    ...IDLE_GATE,
  });
  assert.equal(restoring.available, false);
  assert.equal(restoring.available === false && restoring.reason, 'restore_in_progress');

  const checking = deriveCompareForgetAvailability({
    scope,
    workspace: { ...workspace, retention: { status: 'checking', requestId: 4 } },
    ...IDLE_GATE,
  });
  assert.equal(checking.available, false);
  assert.equal(checking.available === false && checking.reason, 'retention_checking');
});

test('forget fails closed when the scope does not hold the result being discarded', () => {
  const repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  const scope = onlyScope(repository);

  assert.equal(
    deriveCompareForgetAvailability({ scope: null, workspace: scope.active, ...IDLE_GATE }).available,
    false,
  );
  assert.equal(
    deriveCompareForgetAvailability({ scope, workspace: null, ...IDLE_GATE }).available,
    false,
  );
  const foreign = deriveCompareForgetAvailability({
    scope: { ...scope, active: null },
    workspace: scope.active,
    ...IDLE_GATE,
  });
  assert.equal(foreign.available, false);
  assert.equal(foreign.available === false && foreign.reason, 'not_held_by_scope');
});

test('an expired view-only result stays forgettable, because that is what forget is for', () => {
  let repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  repository = dispatch(repository, {
    type: 'job_execution_expired',
    jobId: 'job-a',
    configRevision: 'rev-a',
    reason: 'job_changed',
  });
  const scope = onlyScope(repository);

  assert.equal(
    deriveCompareForgetAvailability({ scope, workspace: scope.active, ...IDLE_GATE }).available,
    true,
  );
});

test('a confirmation that is opened and cancelled discards nothing', () => {
  const repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  const edited = dispatch(repository, {
    type: 'row_inclusion_replaced',
    resultKey: keyA,
    rowIncluded: [false, true],
  });
  const request = { scopeKey: scopeKeyA, resultKey: keyA };

  // Opening the confirmation only resolves the request; the evidence and its review state are
  // removed by the `result_forgotten` transition alone, which cancelling never reaches.
  const resolution = resolveCompareResultForget(edited, request, IDLE_GATE);
  assert.equal(resolution.status, 'forget');

  assert.equal(onlyScope(edited).active?.key, keyA);
  assert.deepEqual(onlyScope(edited).active?.differences.rowIncluded, [false, true]);
});

test('a confirmation resolved after the result changed is refused instead of discarding the newer one', () => {
  let repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  const request = { scopeKey: scopeKeyA, resultKey: keyA };
  repository = publish(repository, planB, 2);

  const resolution = resolveCompareResultForget(repository, request, IDLE_GATE);
  assert.equal(resolution.status, 'refused');
  assert.equal(onlyScope(repository).active?.key, keyB);
});

test('a resolved confirmation carries the exact identity the backend must forget', () => {
  const repository = publish(emptyCompareWorkspaceRepository, planA, 1);

  const resolution = resolveCompareResultForget(
    repository,
    { scopeKey: scopeKeyA, resultKey: keyA },
    IDLE_GATE,
  );
  assert.equal(resolution.status, 'forget');
  assert.deepEqual(
    resolution.status === 'forget' ? resolution.identity : null,
    planA.owner.identity,
  );
});

test('a confirmation resolved while a run started is refused', () => {
  const repository = publish(emptyCompareWorkspaceRepository, planA, 1);

  const resolution = resolveCompareResultForget(
    repository,
    { scopeKey: scopeKeyA, resultKey: keyA },
    { runInFlight: true, reviewPending: false },
  );
  assert.equal(resolution.status, 'refused');
});
