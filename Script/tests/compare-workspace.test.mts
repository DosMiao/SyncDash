import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import test from 'node:test';

import {
  activeWorkspace,
  compareResultKey,
  compareScopeForJob,
  compareScopeFromIdentity,
  compareScopeKey,
  createCompareWorkspace,
  emptyCompareWorkspaceRepository,
  preferredTargetIndex,
  sameCompareIdentity,
  scopeWorkspace,
  workspaceByResultKey,
} from '../../typescript/ui/state/compareWorkspaceModel.ts';
import type {
  CompareWorkspaceRepository,
} from '../../typescript/ui/state/compareWorkspaceModel.ts';
import {
  deriveWorkspaceExecutionAccess,
  reconcileExecutionStatus,
} from '../../typescript/ui/state/compareWorkspaceExecution.ts';
import { reduceCompareWorkspaces } from '../../typescript/ui/state/compareWorkspaceRepository.ts';
import type { CompareWorkspaceAction } from '../../typescript/ui/state/compareWorkspaceRepository.ts';
import type { PlanDto, PlanOperation } from '../../typescript/core/plan.ts';
import type { CompareIdentity } from '../../typescript/core/types/generated/CompareIdentity.ts';
import type { CompareOwner } from '../../typescript/core/types/generated/CompareOwner.ts';
import type { CompareScopeDto } from '../../typescript/core/types/generated/CompareScopeDto.ts';
import type { CompareScopeExecutionStatusDto } from '../../typescript/core/types/generated/CompareScopeExecutionStatusDto.ts';
import type { CompareWorkspaceLookupDto } from '../../typescript/core/types/generated/CompareWorkspaceLookupDto.ts';
import type { CompareWorkspaceSnapshotDto } from '../../typescript/core/types/generated/CompareWorkspaceSnapshotDto.ts';
import type { IdenticalRow } from '../../typescript/core/types/generated/IdenticalRow.ts';
import type { JobDto } from '../../typescript/core/types/generated/JobDto.ts';

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

function owner(
  jobId: string,
  targetIndex: number,
  configRevision: string,
  compareRunId: number,
  jobName = jobId,
): CompareOwner {
  return { identity: identity(jobId, targetIndex, configRevision, compareRunId), job_name: jobName };
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

function plan(
  resultOwner: CompareOwner,
  operations: PlanOperation[] = [
    operation('copy', 'docs/a.txt'),
    operation('conflict', 'docs/conflict.txt'),
    operation('delete', 'archive/old.txt'),
  ],
): PlanDto {
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
      conflict_count: operations.filter((entry) => entry.action === 'conflict').length,
      source_entries: 3,
      target_entries: 3,
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
    },
    ops: operations,
    metas: operations.map(() => null),
    identical_count: 4,
    identical_bytes: 512,
  };
}

function scope(jobId: string, targetIndex: number, configRevision: string): CompareScopeDto {
  return { job_id: jobId, target_index: targetIndex, config_revision: configRevision };
}

function fresh(resultPlan: PlanDto, verificationEpoch: number): CompareScopeExecutionStatusDto {
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

function snapshot(
  resultPlan: PlanDto,
  executionStatus: CompareScopeExecutionStatusDto = fresh(resultPlan, resultPlan.owner.identity.compare_run_id),
): CompareWorkspaceSnapshotDto {
  return { plan: resultPlan, execution_status: executionStatus };
}

function found(resultSnapshot: CompareWorkspaceSnapshotDto): CompareWorkspaceLookupDto {
  return { status: 'found', workspace: resultSnapshot };
}

function missing(
  resultScope: CompareScopeDto,
  executionStatus: CompareScopeExecutionStatusDto = { status: 'unavailable', scope: resultScope },
): CompareWorkspaceLookupDto {
  return { status: 'missing', execution_status: executionStatus };
}

function job(jobId: string, configRevision: string, targets = ['/target'], name = jobId): JobDto {
  return { job_id: jobId, config_revision: configRevision, targets, name } as JobDto;
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
  verificationEpoch = resultPlan.owner.identity.compare_run_id,
): CompareWorkspaceRepository {
  return reduceCompareWorkspaces(repository, {
    type: 'manual_compare_published',
    snapshot: snapshot(resultPlan, fresh(resultPlan, verificationEpoch)),
  });
}

function identicalRow(path: string): IdenticalRow {
  return { path, size: 64, source_mtime_ms: 10, target_mtime_ms: 10 };
}

test('workspace creation validates plan shape and derives executable review defaults', () => {
  const resultPlan = plan(owner('job-a', 0, 'rev-a', 1, 'A'));
  const workspace = createCompareWorkspace(resultPlan);

  assert.deepEqual(workspace.differences.rowIncluded, [true, false, true]);
  assert.deepEqual(workspace.differences.rowReversed, [false, false, false]);
  assert.equal(workspace.plan, resultPlan);
  assert.equal(workspace.differences.pathMode, 'relative');
  assert.throws(
    () => createCompareWorkspace({ ...resultPlan, metas: [] }),
    /exactly one entry per operation/,
  );
});

test('switching jobs and targets preserves the complete exact-result workspace', () => {
  const planA = plan(owner('job-a', 1, 'rev-a', 11, 'A'));
  const planB = plan(owner('job-b', 0, 'rev-b', 21, 'B'));
  const keyA = compareResultKey(planA.owner.identity);
  let repository = publish(emptyCompareWorkspaceRepository, planA, 1);
  repository = dispatch(
    repository,
    { type: 'row_inclusion_replaced', resultKey: keyA, rowIncluded: [false, false, true] },
    { type: 'row_reversal_replaced', resultKey: keyA, rowReversed: [true, false, false] },
    { type: 'selected_result_types_changed', resultKey: keyA, resultTypes: new Set(['copy', 'delete']) },
    { type: 'difference_search_draft_changed', resultKey: keyA, requestId: 1, draft: 'docs' },
    { type: 'difference_search_applied', resultKey: keyA, requestId: 1, query: 'docs' },
    { type: 'folder_scope_changed', resultKey: keyA, folderScope: 'docs' },
    {
      type: 'advanced_filter_applied',
      resultKey: keyA,
      appliedFilter: { masks: ['*.tmp'], minimumMiB: 1, maximumMiB: 10, modifiedWithinDays: 7 },
    },
    { type: 'scope_folder_expansion_toggled', resultKey: keyA, folderPath: 'docs' },
    { type: 'scope_panel_collapsed_changed', resultKey: keyA, collapsed: false },
    { type: 'difference_sort_changed', resultKey: keyA, sort: { key: 's.path', dir: -1 } },
    { type: 'difference_grouping_changed', resultKey: keyA, grouped: false },
    { type: 'path_mode_changed', resultKey: keyA, pathMode: 'full' },
    { type: 'difference_folder_fold_toggled', resultKey: keyA, folderPath: 'archive' },
    { type: 'difference_viewport_changed', resultKey: keyA, viewport: { logicalTop: 37, scrollLeft: 18 } },
    { type: 'result_view_changed', resultKey: keyA, view: 'identical' },
    { type: 'identical_search_draft_changed', resultKey: keyA, requestId: 2, draft: 'same' },
    { type: 'identical_search_applied', resultKey: keyA, requestId: 2, query: 'same' },
    { type: 'identical_initial_load_started', resultKey: keyA, requestId: 3, query: 'same' },
    {
      type: 'identical_page_loaded',
      resultKey: keyA,
      requestId: 3,
      query: 'same',
      offset: 0,
      rows: [identicalRow('same.txt')],
      total: 1,
    },
    { type: 'identical_viewport_changed', resultKey: keyA, viewport: { scrollTop: 91, scrollLeft: 7 } },
  );
  repository = publish(repository, planB, 2);
  repository = reduceCompareWorkspaces(repository, {
    type: 'scope_touched',
    scope: compareScopeFromIdentity(planA.owner.identity),
  });

  const restored = activeWorkspace(repository, job('job-a', 'rev-a', ['/a0', '/a1'], 'A'), 1)!;
  assert.deepEqual(restored.differences.rowIncluded, [false, false, true]);
  assert.deepEqual(restored.differences.rowReversed, [true, false, false]);
  assert.deepEqual([...restored.differences.selectedResultTypes], ['copy', 'delete']);
  assert.equal(restored.differences.appliedSearch, 'docs');
  assert.deepEqual(restored.differences.appliedAdvancedFilter.masks, ['*.tmp']);
  assert.deepEqual(restored.differences.viewport, { logicalTop: 37, scrollLeft: 18 });
  assert.equal(restored.selectedView, 'identical');
  assert.deepEqual(restored.identical.viewport, { scrollTop: 91, scrollLeft: 7 });
  assert.equal(restored.identical.pages.status, 'ready');
  assert.ok(restored.reviewRevision > 0);
});

test('repository retains every reviewed scope and target selection follows the newest scope', () => {
  let repository = emptyCompareWorkspaceRepository;
  for (let index = 0; index < 100; index++) {
    repository = publish(repository, plan(owner(`job-${index}`, 0, `rev-${index}`, index + 1)), index + 1);
  }
  repository = reduceCompareWorkspaces(repository, {
    type: 'scope_touched',
    scope: scope('job-0', 0, 'rev-0'),
  });
  repository = publish(repository, plan(owner('job-new', 0, 'rev-new', 99)), 99);

  assert.equal(repository.scopes.length, 101);
  assert.ok(scopeWorkspace(repository, scope('job-0', 0, 'rev-0')));
  assert.ok(scopeWorkspace(repository, scope('job-1', 0, 'rev-1')));

  const multiTarget = job('multi', 'rev-multi', ['/0', '/1', '/2']);
  repository = publish(repository, plan(owner('multi', 2, 'rev-multi', 100)), 100);
  assert.equal(preferredTargetIndex(repository, multiTarget), 2);
  assert.equal(preferredTargetIndex(repository, job('multi', 'other', ['/0', '/1', '/2'])), 0);
});

test('execution reconciliation is scope-fenced and monotonic by verification epoch', () => {
  const executionScope = scope('job-a', 0, 'rev-a');
  const awaiting: CompareScopeExecutionStatusDto = {
    status: 'awaiting_compare',
    scope: executionScope,
    attempt: { verification_epoch: 2, compare_run_id: null },
  };
  const comparing: CompareScopeExecutionStatusDto = {
    status: 'comparing',
    scope: executionScope,
    attempt: { verification_epoch: 2, compare_run_id: 8 },
  };
  const resultPlan = plan(owner('job-a', 0, 'rev-a', 8));
  const currentFresh = fresh(resultPlan, 2);

  assert.equal(reconcileExecutionStatus(null, awaiting), awaiting);
  assert.equal(reconcileExecutionStatus(awaiting, comparing), comparing);
  assert.equal(reconcileExecutionStatus(comparing, currentFresh), currentFresh);
  assert.equal(reconcileExecutionStatus(currentFresh, comparing), currentFresh);
  assert.equal(
    reconcileExecutionStatus(currentFresh, fresh(plan(owner('job-a', 0, 'rev-a', 7)), 1)),
    currentFresh,
  );
  assert.equal(
    reconcileExecutionStatus(currentFresh, { status: 'unavailable', scope: executionScope }),
    currentFresh,
  );
  const expired: CompareScopeExecutionStatusDto = {
    status: 'expired',
    scope: executionScope,
    attempt: currentFresh.attempt,
    reason: 'application_restarted',
  };
  assert.equal(reconcileExecutionStatus(currentFresh, expired), expired);
  assert.equal(
    reconcileExecutionStatus(expired, fresh(resultPlan, 2)),
    expired,
  );
  assert.equal(
    reconcileExecutionStatus(currentFresh, fresh(plan(owner('job-b', 0, 'rev-b', 9)), 3)),
    currentFresh,
  );
});

test('execution access preserves view-only results and authorizes only the exact fresh identity', () => {
  const olderPlan = plan(owner('job-a', 0, 'rev-a', 1));
  const newerPlan = plan(owner('job-a', 0, 'rev-a', 2));
  const workspace = createCompareWorkspace(olderPlan);
  assert.deepEqual(deriveWorkspaceExecutionAccess(workspace, fresh(olderPlan, 1)), {
    status: 'executable',
    verificationEpoch: 1,
  });
  assert.deepEqual(deriveWorkspaceExecutionAccess(workspace, fresh(newerPlan, 2)), {
    status: 'view_only',
    reason: 'superseded',
    replacement: newerPlan.owner.identity,
  });
  assert.equal(deriveWorkspaceExecutionAccess(workspace, null).status, 'view_only');
  assert.equal(deriveWorkspaceExecutionAccess(workspace, {
    status: 'cancelled',
    scope: compareScopeFromIdentity(olderPlan.owner.identity),
    attempt: { verification_epoch: 2, compare_run_id: 3 },
  }).status, 'view_only');
});

test('AutoScan stages newer candidates without replacing active review state', () => {
  const first = plan(owner('job-a', 0, 'rev-a', 1));
  const second = plan(owner('job-a', 0, 'rev-a', 2));
  const third = plan(owner('job-a', 0, 'rev-a', 3));
  let repository = publish(emptyCompareWorkspaceRepository, first, 1);
  const firstKey = compareResultKey(first.owner.identity);
  repository = reduceCompareWorkspaces(repository, {
    type: 'row_inclusion_replaced',
    resultKey: firstKey,
    rowIncluded: [false, false, true],
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(second, fresh(second, 2)),
    generation: 4,
    ticketId: 20,
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(third, fresh(third, 3)),
    generation: 5,
    ticketId: 21,
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(second, fresh(second, 2)),
    generation: 4,
    ticketId: 19,
  });

  const retainedScope = scopeWorkspace(repository, compareScopeFromIdentity(first.owner.identity))!;
  assert.equal(retainedScope.active?.key, firstKey);
  assert.deepEqual(retainedScope.active?.differences.rowIncluded, [false, false, true]);
  assert.equal(retainedScope.candidate?.workspace.key, compareResultKey(third.owner.identity));
  assert.deepEqual(retainedScope.candidate?.origin, { kind: 'auto_scan', generation: 5, ticketId: 21 });
});

test('candidate adoption and dismissal are exact-result fenced', () => {
  const first = plan(owner('job-a', 0, 'rev-a', 1));
  const second = plan(owner('job-a', 0, 'rev-a', 2));
  const third = plan(owner('job-a', 0, 'rev-a', 3));
  let repository = publish(emptyCompareWorkspaceRepository, first, 1);
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(second, fresh(second, 2)),
    generation: 1,
    ticketId: 1,
  });
  const scopeKey = compareScopeKey(compareScopeFromIdentity(first.owner.identity));
  repository = reduceCompareWorkspaces(repository, {
    type: 'candidate_discarded',
    scopeKey,
    expectedResultKey: compareResultKey(third.owner.identity),
  });
  assert.ok(scopeWorkspace(repository, compareScopeFromIdentity(first.owner.identity))?.candidate);
  repository = reduceCompareWorkspaces(repository, {
    type: 'candidate_discarded',
    scopeKey,
    expectedResultKey: compareResultKey(second.owner.identity),
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(second, fresh(second, 2)),
    generation: 1,
    ticketId: 1,
  });
  assert.equal(scopeWorkspace(repository, compareScopeFromIdentity(first.owner.identity))?.candidate, null);
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(third, fresh(third, 3)),
    generation: 2,
    ticketId: 2,
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'candidate_adopted',
    scopeKey,
    expectedResultKey: compareResultKey(third.owner.identity),
  });
  assert.equal(
    scopeWorkspace(repository, compareScopeFromIdentity(first.owner.identity))?.active?.key,
    compareResultKey(third.owner.identity),
  );
});

test('AutoScan publication ordering survives candidate adoption, dismissal, and manual replacement', () => {
  const first = plan(owner('job-a', 0, 'rev-a', 1));
  const adopted = plan(owner('job-a', 0, 'rev-a', 3));
  const delayed = plan(owner('job-a', 0, 'rev-a', 4));
  const scopeKey = compareScopeKey(compareScopeFromIdentity(first.owner.identity));
  let repository = publish(emptyCompareWorkspaceRepository, first, 1);
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(adopted, fresh(adopted, 3)),
    generation: 5,
    ticketId: 21,
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'candidate_adopted',
    scopeKey,
    expectedResultKey: compareResultKey(adopted.owner.identity),
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(delayed, fresh(delayed, 4)),
    generation: 4,
    ticketId: 20,
  });
  let retainedScope = scopeWorkspace(repository, compareScopeFromIdentity(first.owner.identity))!;
  assert.equal(retainedScope.active?.key, compareResultKey(adopted.owner.identity));
  assert.equal(retainedScope.candidate, null);
  assert.deepEqual(retainedScope.latestAutoScanPublication, {
    generation: 5,
    ticketId: 21,
    resultKey: compareResultKey(adopted.owner.identity),
  });

  const manual = plan(owner('job-a', 0, 'rev-a', 5));
  repository = publish(repository, manual, 5);
  const contradictory = plan(owner('job-a', 0, 'rev-a', 6));
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(contradictory, fresh(contradictory, 6)),
    generation: 5,
    ticketId: 21,
  });
  retainedScope = scopeWorkspace(repository, compareScopeFromIdentity(first.owner.identity))!;
  assert.equal(retainedScope.active?.key, compareResultKey(manual.owner.identity));
  assert.equal(retainedScope.candidate, null);

  const dismissed = plan(owner('job-a', 0, 'rev-a', 7));
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(dismissed, fresh(dismissed, 7)),
    generation: 6,
    ticketId: 22,
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'candidate_discarded',
    scopeKey,
    expectedResultKey: compareResultKey(dismissed.owner.identity),
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: snapshot(delayed, fresh(delayed, 8)),
    generation: 5,
    ticketId: 20,
  });
  retainedScope = scopeWorkspace(repository, compareScopeFromIdentity(first.owner.identity))!;
  assert.equal(retainedScope.candidate, null);
  assert.equal(retainedScope.latestAutoScanPublication?.ticketId, 22);
});

test('successful publication requires one exact fresh plan and execution identity', () => {
  const first = plan(owner('job-a', 0, 'rev-a', 1));
  const second = plan(owner('job-a', 0, 'rev-a', 2));
  const mismatchedSnapshot = snapshot(first, fresh(second, 2));
  let repository = reduceCompareWorkspaces(emptyCompareWorkspaceRepository, {
    type: 'manual_compare_published',
    snapshot: mismatchedSnapshot,
  });
  assert.equal(repository, emptyCompareWorkspaceRepository);
  repository = reduceCompareWorkspaces(repository, {
    type: 'autoscan_compare_published',
    snapshot: {
      plan: first,
      execution_status: {
        status: 'failed',
        scope: compareScopeFromIdentity(first.owner.identity),
        attempt: { verification_epoch: 1, compare_run_id: 1 },
        message: 'failed',
      },
    },
    generation: 1,
    ticketId: 1,
  });
  assert.equal(repository, emptyCompareWorkspaceRepository);
});

test('exact workspace lookup fences stale responses and preserves review state when evidence is missing', () => {
  const resultPlan = plan(owner('job-a', 0, 'rev-a', 7, 'A'));
  const resultKey = compareResultKey(resultPlan.owner.identity);
  let repository = publish(emptyCompareWorkspaceRepository, resultPlan, 1);
  repository = dispatch(
    repository,
    { type: 'row_inclusion_replaced', resultKey, rowIncluded: [false, false, true] },
    { type: 'workspace_lookup_started', workspace: workspaceByResultKey(repository, resultKey)!, requestId: 10 },
    {
      type: 'workspace_lookup_completed',
      resultKey,
      requestId: 9,
      lookup: missing(compareScopeFromIdentity(resultPlan.owner.identity)),
    },
  );
  assert.equal(workspaceByResultKey(repository, resultKey)?.retention.status, 'checking');
  repository = reduceCompareWorkspaces(repository, {
    type: 'workspace_lookup_completed',
    resultKey,
    requestId: 10,
    lookup: missing(compareScopeFromIdentity(resultPlan.owner.identity), {
      status: 'expired',
      scope: compareScopeFromIdentity(resultPlan.owner.identity),
      attempt: { verification_epoch: 1, compare_run_id: 7 },
      reason: 'application_restarted',
    }),
  });
  const missingWorkspace = workspaceByResultKey(repository, resultKey)!;
  assert.equal(missingWorkspace.retention.status, 'missing');
  assert.equal(scopeWorkspace(repository, compareScopeFromIdentity(resultPlan.owner.identity))?.execution?.status, 'expired');
  assert.deepEqual(missingWorkspace.differences.rowIncluded, [false, false, true]);
  assert.equal(missingWorkspace.plan, resultPlan);

  repository = dispatch(
    repository,
    { type: 'workspace_lookup_started', workspace: workspaceByResultKey(repository, resultKey)!, requestId: 11 },
    {
      type: 'workspace_lookup_completed',
      resultKey,
      requestId: 11,
      lookup: found(snapshot({ ...resultPlan, owner: { ...resultPlan.owner, job_name: 'Renamed' } }, fresh(resultPlan, 1))),
    },
  );
  assert.equal(workspaceByResultKey(repository, resultKey)?.retention.status, 'retained');
  assert.equal(workspaceByResultKey(repository, resultKey)?.display.jobName, 'Renamed');
  assert.deepEqual(workspaceByResultKey(repository, resultKey)?.differences.rowIncluded, [false, false, true]);
});

test('exact lookup preserves captured review state but never displaces a newer active result', () => {
  const capturedPlan = plan(owner('job-a', 0, 'rev-a', 1));
  const capturedKey = compareResultKey(capturedPlan.owner.identity);
  let repository = publish(emptyCompareWorkspaceRepository, capturedPlan, 1);
  repository = reduceCompareWorkspaces(repository, {
    type: 'row_inclusion_replaced',
    resultKey: capturedKey,
    rowIncluded: [false, false, true],
  });
  const captured = workspaceByResultKey(repository, capturedKey)!;
  assert.ok(workspaceByResultKey(repository, capturedKey));
  repository = dispatch(
    repository,
    { type: 'workspace_lookup_started', workspace: captured, requestId: 10 },
    {
      type: 'workspace_lookup_completed',
      resultKey: capturedKey,
      requestId: 10,
      lookup: found(snapshot(capturedPlan, fresh(capturedPlan, 1))),
    },
  );
  assert.deepEqual(workspaceByResultKey(repository, capturedKey)?.differences.rowIncluded, [false, false, true]);

  const newerPlan = plan(owner('job-a', 0, 'rev-a', 2));
  repository = reduceCompareWorkspaces(repository, {
    type: 'workspace_lookup_started',
    workspace: workspaceByResultKey(repository, capturedKey)!,
    requestId: 11,
  });
  repository = publish(repository, newerPlan, 2);
  repository = reduceCompareWorkspaces(repository, {
    type: 'workspace_lookup_completed',
    resultKey: capturedKey,
    requestId: 11,
    lookup: found(snapshot(capturedPlan, fresh(capturedPlan, 1))),
  });
  assert.equal(
    scopeWorkspace(repository, compareScopeFromIdentity(newerPlan.owner.identity))?.active?.key,
    compareResultKey(newerPlan.owner.identity),
  );
});

test('scope restoration uses exact request and scope fences for found and missing outcomes', () => {
  const restoredPlan = plan(owner('job-a', 0, 'rev-a', 5));
  const restoredScope = compareScopeFromIdentity(restoredPlan.owner.identity);
  const scopeKey = compareScopeKey(restoredScope);
  let repository = dispatch(
    emptyCompareWorkspaceRepository,
    { type: 'scope_restore_started', scope: restoredScope, requestId: 1 },
    { type: 'scope_restore_started', scope: restoredScope, requestId: 2 },
    {
      type: 'scope_restore_completed',
      scopeKey,
      requestId: 1,
      lookup: found(snapshot(restoredPlan, fresh(restoredPlan, 1))),
    },
  );
  assert.equal(scopeWorkspace(repository, restoredScope)?.active, null);
  repository = reduceCompareWorkspaces(repository, {
    type: 'scope_restore_completed',
    scopeKey,
    requestId: 2,
    lookup: found(snapshot(restoredPlan, fresh(restoredPlan, 1))),
  });
  assert.equal(scopeWorkspace(repository, restoredScope)?.active?.key, compareResultKey(restoredPlan.owner.identity));

  repository = dispatch(
    repository,
    { type: 'scope_restore_started', scope: restoredScope, requestId: 3 },
    {
      type: 'scope_restore_completed',
      scopeKey,
      requestId: 3,
      lookup: missing(restoredScope),
    },
  );
  assert.equal(scopeWorkspace(repository, restoredScope)?.active?.retention.status, 'missing');

  const newerPlan = plan(owner('job-a', 0, 'rev-a', 6));
  repository = reduceCompareWorkspaces(repository, {
    type: 'scope_restore_started', scope: restoredScope, requestId: 4,
  });
  repository = publish(repository, newerPlan, 2);
  repository = reduceCompareWorkspaces(repository, {
    type: 'scope_restore_completed',
    scopeKey,
    requestId: 4,
    lookup: missing(restoredScope),
  });
  assert.equal(scopeWorkspace(repository, restoredScope)?.active?.key, compareResultKey(newerPlan.owner.identity));
  assert.equal(scopeWorkspace(repository, restoredScope)?.active?.retention.status, 'retained');
});

test('wrong-scope restoration responses terminate explicitly instead of remaining loading', () => {
  const requestedScope = scope('job-a', 0, 'rev-a');
  const scopeKey = compareScopeKey(requestedScope);
  const wrongScope = scope('job-a', 0, 'rev-b');
  let repository = dispatch(
    emptyCompareWorkspaceRepository,
    { type: 'scope_restore_started', scope: requestedScope, requestId: 1 },
    {
      type: 'scope_restore_completed',
      scopeKey,
      requestId: 1,
      lookup: missing(wrongScope),
    },
  );
  assert.equal(scopeWorkspace(repository, requestedScope)?.restoration.status, 'failed');
  const wrongPlan = plan(owner('job-a', 0, 'rev-b', 3));
  repository = dispatch(
    repository,
    { type: 'scope_restore_started', scope: requestedScope, requestId: 2 },
    {
      type: 'scope_restore_completed',
      scopeKey,
      requestId: 2,
      lookup: found(snapshot(wrongPlan, fresh(wrongPlan, 2))),
    },
  );
  assert.equal(scopeWorkspace(repository, requestedScope)?.restoration.status, 'failed');
});

test('internally inconsistent lookup responses fail closed for restore and exact reconciliation', () => {
  const requestedScope = scope('job-a', 0, 'rev-a');
  const scopeKey = compareScopeKey(requestedScope);
  let repository = dispatch(
    emptyCompareWorkspaceRepository,
    { type: 'scope_restore_started', scope: requestedScope, requestId: 1 },
    {
      type: 'scope_restore_completed',
      scopeKey,
      requestId: 1,
      lookup: missing(requestedScope, {
        status: 'awaiting_compare',
        scope: requestedScope,
        attempt: { verification_epoch: 1, compare_run_id: 9 },
      }),
    },
  );
  assert.equal(scopeWorkspace(repository, requestedScope)?.restoration.status, 'failed');

  const resultPlan = plan(owner('job-a', 0, 'rev-a', 9));
  const resultKey = compareResultKey(resultPlan.owner.identity);
  repository = publish(repository, resultPlan, 1);
  repository = dispatch(
    repository,
    { type: 'workspace_lookup_started', workspace: workspaceByResultKey(repository, resultKey)!, requestId: 2 },
    {
      type: 'workspace_lookup_completed',
      resultKey,
      requestId: 2,
      lookup: found(snapshot(resultPlan, {
        status: 'fresh',
        scope: requestedScope,
        attempt: { verification_epoch: 1, compare_run_id: 11 },
        owner: owner('job-a', 0, 'rev-a', 10),
      })),
    },
  );
  assert.equal(workspaceByResultKey(repository, resultKey)?.retention.status, 'check_failed');
});

test('transient restoration scopes never discard retained evidence', () => {
  let repository = emptyCompareWorkspaceRepository;
  for (let index = 0; index < 100; index += 1) {
    repository = publish(
      repository,
      plan(owner(`job-${index}`, 0, `rev-${index}`, index + 1)),
      index + 1,
    );
  }
  const oldestScope = scope('job-0', 0, 'rev-0');
  const transientScope = scope('no-result', 0, 'rev-none');
  repository = reduceCompareWorkspaces(repository, {
    type: 'scope_restore_started',
    scope: transientScope,
    requestId: 99,
  });
  assert.ok(scopeWorkspace(repository, oldestScope)?.active);
  assert.equal(repository.scopes.filter((entry) => entry.active || entry.candidate).length, 100);
  repository = reduceCompareWorkspaces(repository, {
    type: 'scope_restore_completed',
    scopeKey: compareScopeKey(transientScope),
    requestId: 99,
    lookup: missing(transientScope),
  });
  assert.ok(scopeWorkspace(repository, oldestScope)?.active);
});

test('execution expiry and an active Compare reject stale row-edit actions in the reducer', () => {
  const resultPlan = plan(owner('job-a', 0, 'rev-a', 1));
  const resultKey = compareResultKey(resultPlan.owner.identity);
  const resultScope = compareScopeFromIdentity(resultPlan.owner.identity);
  let repository = publish(emptyCompareWorkspaceRepository, resultPlan, 1);
  repository = reduceCompareWorkspaces(repository, {
    type: 'execution_status_received',
    execution: {
      status: 'expired',
      scope: resultScope,
      attempt: { verification_epoch: 1, compare_run_id: 1 },
      reason: 'write_started',
    },
  });
  const beforeExpiryEdit = workspaceByResultKey(repository, resultKey)!;
  repository = dispatch(
    repository,
    { type: 'row_inclusion_replaced', resultKey, rowIncluded: [false, false, false] },
    { type: 'row_reversal_replaced', resultKey, rowReversed: [true, false, false] },
  );
  assert.equal(workspaceByResultKey(repository, resultKey), beforeExpiryEdit);

  const newerPlan = plan(owner('job-b', 0, 'rev-b', 2));
  repository = publish(repository, newerPlan, 2);
  const newerKey = compareResultKey(newerPlan.owner.identity);
  repository = reduceCompareWorkspaces(repository, {
    type: 'compare_activity_started',
    scope: compareScopeFromIdentity(newerPlan.owner.identity),
    requestId: 1,
    origin: { kind: 'interactive' },
  });
  const beforeActiveCompareEdit = workspaceByResultKey(repository, newerKey)!;
  repository = reduceCompareWorkspaces(repository, {
    type: 'row_inclusion_replaced',
    resultKey: newerKey,
    rowIncluded: [false, false, false],
  });
  assert.equal(workspaceByResultKey(repository, newerKey), beforeActiveCompareEdit);
});

test('advanced filters publish atomically and reject invalid scope criteria', () => {
  const resultPlan = plan(owner('job-a', 0, 'rev-a', 1));
  const resultKey = compareResultKey(resultPlan.owner.identity);
  const repository = publish(emptyCompareWorkspaceRepository, resultPlan, 1);
  const originalWorkspace = workspaceByResultKey(repository, resultKey)!;

  assert.throws(() => reduceCompareWorkspaces(repository, {
    type: 'advanced_filter_applied',
    resultKey,
    appliedFilter: { masks: ['*.log'], minimumMiB: 8, maximumMiB: 4, modifiedWithinDays: null },
  }), /Advanced filter is invalid/);
  assert.equal(workspaceByResultKey(repository, resultKey), originalWorkspace);

  const applied = reduceCompareWorkspaces(repository, {
    type: 'advanced_filter_applied',
    resultKey,
    appliedFilter: {
      masks: ['  *.log  ', '', '/cache/'],
      minimumMiB: 1,
      maximumMiB: 8,
      modifiedWithinDays: 7,
    },
  });
  const appliedWorkspace = workspaceByResultKey(applied, resultKey)!;
  assert.deepEqual(appliedWorkspace.differences.appliedAdvancedFilter, {
    masks: ['*.log', '/cache/'],
    minimumMiB: 1,
    maximumMiB: 8,
    modifiedWithinDays: 7,
  });
  assert.equal(appliedWorkspace.reviewRevision, originalWorkspace.reviewRevision + 1);
  assert.equal(appliedWorkspace.differences.maskInputRevision, 1);
  assert.deepEqual(appliedWorkspace.differences.maskResolution, {
    status: 'unresolved',
    inputRevision: 1,
  });

  const unchanged = reduceCompareWorkspaces(applied, {
    type: 'advanced_filter_applied',
    resultKey,
    appliedFilter: {
      masks: ['*.log', '/cache/'],
      minimumMiB: 1,
      maximumMiB: 8,
      modifiedWithinDays: 7,
    },
  });
  assert.equal(unchanged, applied);
});

test('mask resolution is fenced by exact result, input revision, request, and row count', () => {
  const resultPlan = plan(owner('job-a', 0, 'rev-a', 1));
  const resultKey = compareResultKey(resultPlan.owner.identity);
  let repository = publish(emptyCompareWorkspaceRepository, resultPlan, 1);
  repository = reduceCompareWorkspaces(repository, {
    type: 'advanced_filter_applied',
    resultKey,
    appliedFilter: { masks: ['*.log'], minimumMiB: null, maximumMiB: null, modifiedWithinDays: null },
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'mask_resolution_started', resultKey, inputRevision: 1, requestId: 7,
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'row_reversal_replaced', resultKey, rowReversed: [true, false, false],
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'mask_resolution_succeeded',
    resultKey,
    inputRevision: 1,
    requestId: 7,
    excludedByRow: [true, false, false],
  });
  assert.equal(workspaceByResultKey(repository, resultKey)?.differences.maskResolution.status, 'unresolved');
  repository = dispatch(
    repository,
    { type: 'mask_resolution_started', resultKey, inputRevision: 2, requestId: 8 },
    {
      type: 'mask_resolution_succeeded',
      resultKey,
      inputRevision: 2,
      requestId: 8,
      excludedByRow: [true],
    },
  );
  assert.equal(workspaceByResultKey(repository, resultKey)?.differences.maskResolution.status, 'pending');
  repository = reduceCompareWorkspaces(repository, {
    type: 'mask_resolution_succeeded',
    resultKey,
    inputRevision: 2,
    requestId: 8,
    excludedByRow: [true, false, false],
  });
  assert.deepEqual(workspaceByResultKey(repository, resultKey)?.differences.maskResolution, {
    status: 'ready',
    inputRevision: 2,
    requestId: 8,
    excludedByRow: [true, false, false],
  });
});

test('Identical pagination unions pages and preserves loaded evidence on load-more failure', () => {
  const resultPlan = plan(owner('job-a', 0, 'rev-a', 1));
  const resultKey = compareResultKey(resultPlan.owner.identity);
  let repository = publish(emptyCompareWorkspaceRepository, resultPlan, 1);
  repository = dispatch(
    repository,
    { type: 'identical_search_draft_changed', resultKey, requestId: 1, draft: 'docs' },
    { type: 'identical_search_applied', resultKey, requestId: 1, query: 'docs' },
    { type: 'identical_initial_load_started', resultKey, requestId: 10, query: 'docs' },
    {
      type: 'identical_page_loaded',
      resultKey,
      requestId: 9,
      query: 'docs',
      offset: 0,
      rows: [identicalRow('stale.txt')],
      total: 2,
    },
    {
      type: 'identical_page_loaded',
      resultKey,
      requestId: 10,
      query: 'docs',
      offset: 0,
      rows: [identicalRow('docs/a.txt')],
      total: 2,
    },
    { type: 'identical_load_more_started', resultKey, requestId: 11, query: 'docs', offset: 1 },
    {
      type: 'identical_page_failed',
      resultKey,
      requestId: 11,
      query: 'docs',
      error: 'offline',
    },
  );
  let pages = workspaceByResultKey(repository, resultKey)?.identical.pages;
  assert.equal(pages?.status, 'load_more_failed');
  if (pages?.status === 'load_more_failed') assert.deepEqual(pages.rows.map((row) => row.path), ['docs/a.txt']);

  repository = dispatch(
    repository,
    { type: 'identical_load_more_started', resultKey, requestId: 12, query: 'docs', offset: 1 },
    {
      type: 'identical_page_loaded',
      resultKey,
      requestId: 12,
      query: 'docs',
      offset: 1,
      rows: [identicalRow('docs/b.txt')],
      total: 2,
    },
  );
  pages = workspaceByResultKey(repository, resultKey)?.identical.pages;
  assert.equal(pages?.status, 'ready');
  if (pages?.status === 'ready') assert.deepEqual(pages.rows.map((row) => row.path), ['docs/a.txt', 'docs/b.txt']);
});

test('compare activity transitions and async completions are request-fenced', () => {
  const activityScope = scope('job-a', 0, 'rev-a');
  const scopeKey = compareScopeKey(activityScope);
  let repository = reduceCompareWorkspaces(emptyCompareWorkspaceRepository, {
    type: 'compare_activity_started',
    scope: activityScope,
    requestId: 5,
    origin: { kind: 'interactive' },
  });
  repository = reduceCompareWorkspaces(repository, {
    type: 'compare_activity_finished', scopeKey, requestId: 4,
  });
  assert.equal(scopeWorkspace(repository, activityScope)?.activity.status, 'comparing');
  repository = dispatch(
    repository,
    { type: 'compare_activity_failed', scopeKey, requestId: 5, error: 'cancel failed' },
  );
  assert.equal(scopeWorkspace(repository, activityScope)?.activity.status, 'failed');
  repository = reduceCompareWorkspaces(repository, {
    type: 'compare_activity_finished', scopeKey, requestId: 5,
  });
  assert.equal(scopeWorkspace(repository, activityScope)?.activity.status, 'idle');
});

test('job renames update display names without mutating immutable result evidence', () => {
  const oldPlan = plan(owner('job-a', 0, 'rev-old', 1, 'Old'));
  const currentPlan = plan(owner('job-a', 0, 'rev-current', 2, 'Old'));
  const otherPlan = plan(owner('job-b', 0, 'rev-b', 3, 'B'));
  let repository = publish(emptyCompareWorkspaceRepository, oldPlan, 1);
  repository = publish(repository, currentPlan, 2);
  repository = publish(repository, otherPlan, 3);
  const immutablePlan = workspaceByResultKey(repository, compareResultKey(currentPlan.owner.identity))?.plan;
  repository = reduceCompareWorkspaces(repository, {
    type: 'job_display_name_rebound', jobId: 'job-a', jobName: 'Renamed',
  });
  const renamed = workspaceByResultKey(repository, compareResultKey(currentPlan.owner.identity))!;
  assert.equal(renamed.display.jobName, 'Renamed');
  assert.equal(renamed.plan, immutablePlan);
  assert.equal(renamed.plan.owner.job_name, 'Old');
  assert.equal(
    workspaceByResultKey(repository, compareResultKey(otherPlan.owner.identity))?.display.jobName,
    'B',
  );
});

test('job revision and deletion expiry preserve evidence while invalidated scopes become view-only', () => {
  const activePlan = plan(owner('job-a', 0, 'rev-old', 1, 'Old'));
  const candidatePlan = plan(owner('job-a', 0, 'rev-old', 2, 'Old'));
  const currentPlan = plan(owner('job-a', 0, 'rev-current', 3, 'Current'));
  const otherPlan = plan(owner('job-b', 0, 'rev-b', 4, 'Other'));
  const activeKey = compareResultKey(activePlan.owner.identity);
  const candidateKey = compareResultKey(candidatePlan.owner.identity);
  const oldScopeIdentity = compareScopeFromIdentity(activePlan.owner.identity);
  const currentScopeIdentity = compareScopeFromIdentity(currentPlan.owner.identity);
  const otherScopeIdentity = compareScopeFromIdentity(otherPlan.owner.identity);
  let repository = publish(emptyCompareWorkspaceRepository, activePlan, 1);
  repository = dispatch(
    repository,
    { type: 'row_inclusion_replaced', resultKey: activeKey, rowIncluded: [false, false, true] },
    { type: 'result_view_changed', resultKey: activeKey, view: 'identical' },
    { type: 'identical_search_draft_changed', resultKey: activeKey, requestId: 1, draft: 'docs' },
    { type: 'identical_search_applied', resultKey: activeKey, requestId: 1, query: 'docs' },
    { type: 'identical_initial_load_started', resultKey: activeKey, requestId: 2, query: 'docs' },
    {
      type: 'identical_page_loaded',
      resultKey: activeKey,
      requestId: 2,
      query: 'docs',
      offset: 0,
      rows: [identicalRow('docs/a.txt')],
      total: 1,
    },
    { type: 'identical_viewport_changed', resultKey: activeKey, viewport: { scrollTop: 91, scrollLeft: 7 } },
    {
      type: 'autoscan_compare_published',
      snapshot: snapshot(candidatePlan, fresh(candidatePlan, 2)),
      generation: 1,
      ticketId: 1,
    },
    { type: 'result_view_changed', resultKey: candidateKey, view: 'identical' },
    { type: 'identical_viewport_changed', resultKey: candidateKey, viewport: { scrollTop: 45, scrollLeft: 6 } },
  );
  repository = publish(repository, currentPlan, 3);
  repository = publish(repository, otherPlan, 4);

  const oldScopeBefore = scopeWorkspace(repository, oldScopeIdentity)!;
  const activeBefore = oldScopeBefore.active!;
  const candidateBefore = oldScopeBefore.candidate!.workspace;
  const currentScopeBefore = scopeWorkspace(repository, currentScopeIdentity)!;
  const otherScopeBefore = scopeWorkspace(repository, otherScopeIdentity)!;
  assert.equal(oldScopeBefore.execution?.status, 'fresh');

  repository = reduceCompareWorkspaces(repository, {
    type: 'job_execution_expired',
    jobId: 'job-a',
    configRevision: 'rev-old',
    reason: 'job_changed',
  });

  const oldScopeAfter = scopeWorkspace(repository, oldScopeIdentity)!;
  assert.equal(oldScopeAfter.active, activeBefore);
  assert.equal(oldScopeAfter.candidate?.workspace, candidateBefore);
  assert.equal(oldScopeAfter.active?.retention.status, 'retained');
  assert.equal(oldScopeAfter.candidate?.workspace.retention.status, 'retained');
  assert.deepEqual(oldScopeAfter.active?.differences.rowIncluded, [false, false, true]);
  assert.equal(oldScopeAfter.active?.selectedView, 'identical');
  assert.deepEqual(oldScopeAfter.active?.identical.viewport, { scrollTop: 91, scrollLeft: 7 });
  assert.equal(oldScopeAfter.active?.identical.pages.status, 'ready');
  assert.equal(oldScopeAfter.candidate?.workspace.selectedView, 'identical');
  assert.deepEqual(oldScopeAfter.candidate?.workspace.identical.viewport, { scrollTop: 45, scrollLeft: 6 });
  assert.deepEqual(oldScopeAfter.execution, {
    status: 'expired',
    scope: oldScopeBefore.execution?.scope,
    attempt: oldScopeBefore.execution?.attempt,
    reason: 'job_changed',
  });
  assert.deepEqual(deriveWorkspaceExecutionAccess(activeBefore, oldScopeAfter.execution), {
    status: 'view_only',
    reason: 'execution_expired',
    replacement: null,
  });
  assert.deepEqual(deriveWorkspaceExecutionAccess(candidateBefore, oldScopeAfter.execution), {
    status: 'view_only',
    reason: 'execution_expired',
    replacement: null,
  });
  assert.equal(scopeWorkspace(repository, currentScopeIdentity), currentScopeBefore);
  assert.equal(scopeWorkspace(repository, otherScopeIdentity), otherScopeBefore);

  const expiredOldScope = oldScopeAfter;
  const currentWorkspace = currentScopeBefore.active!;
  repository = reduceCompareWorkspaces(repository, {
    type: 'job_execution_expired',
    jobId: 'job-a',
    reason: 'job_deleted',
  });

  const deletedOldScope = scopeWorkspace(repository, oldScopeIdentity)!;
  const deletedCurrentScope = scopeWorkspace(repository, currentScopeIdentity)!;
  assert.equal(deletedOldScope, expiredOldScope);
  assert.equal(deletedOldScope.execution?.status, 'expired');
  if (deletedOldScope.execution?.status === 'expired') {
    assert.equal(deletedOldScope.execution.reason, 'job_changed');
  }
  assert.equal(deletedCurrentScope.active, currentWorkspace);
  assert.equal(deletedCurrentScope.execution?.status, 'expired');
  if (deletedCurrentScope.execution?.status === 'expired') {
    assert.equal(deletedCurrentScope.execution.reason, 'job_deleted');
  }
  assert.deepEqual(deriveWorkspaceExecutionAccess(currentWorkspace, deletedCurrentScope.execution), {
    status: 'view_only',
    reason: 'execution_expired',
    replacement: null,
  });
  assert.equal(scopeWorkspace(repository, otherScopeIdentity), otherScopeBefore);
  assert.ok(workspaceByResultKey(repository, activeKey));
  assert.ok(workspaceByResultKey(repository, candidateKey));
  assert.ok(workspaceByResultKey(repository, compareResultKey(currentPlan.owner.identity)));
});

test('result and scope selectors use stable identity rather than mutable display names', () => {
  const resultPlan = plan(owner('stable-id', 1, 'rev-a', 4, 'Original'));
  const repository = publish(emptyCompareWorkspaceRepository, resultPlan, 1);
  assert.ok(activeWorkspace(repository, job('stable-id', 'rev-a', ['/0', '/1'], 'Renamed'), 1));
  assert.equal(activeWorkspace(repository, job('replacement-id', 'rev-a', ['/0', '/1'], 'Original'), 1), null);
  assert.equal(activeWorkspace(repository, job('stable-id', 'rev-new', ['/0', '/1'], 'Renamed'), 1), null);
  assert.equal(compareScopeKey(compareScopeForJob(job('stable-id', 'rev-a', ['/0', '/1']), 1)), compareScopeKey(scope('stable-id', 1, 'rev-a')));
});

test('result IDs prevent a reused Compare run number from colliding after restart', () => {
  const beforeRestart = identity('stable-id', 1, 'rev-a', 4);
  const afterRestart = {
    ...beforeRestart,
    result_id: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  };
  assert.notEqual(compareResultKey(beforeRestart), compareResultKey(afterRestart));
  assert.equal(sameCompareIdentity(beforeRestart, afterRestart), false);
  assert.throws(
    () => compareResultKey({ ...beforeRestart, result_id: 'not-a-result-id' }),
    /canonical 128-bit hexadecimal value/,
  );
});
