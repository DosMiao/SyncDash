import assert from 'node:assert/strict';
import test from 'node:test';

import {
  buildRunScopeModel,
  computeExecutableIndices,
  computeInScopeIndices,
  EMPTY_ADVANCED_SCOPE_FILTER,
  isMaskMatchResult,
  matchesFolderScope,
  parseScopeMasks,
} from '#core/domain/compare/runScope.ts';
import {
  RESULT_TYPES,
  resultTypeOf,
  describeRowAction,
  effectiveOperation,
  rowTransferBytes,
} from '#core/domain/compare/plan.ts';
import type { PlanDto, PlanOperation, ResultType } from '#core/domain/compare/plan.ts';
import {
  deriveApplyAvailability,
} from '#core/application/apply/applyAvailability.ts';
import {
  loadCompareWorkspacePreferences,
  saveCompareWorkspacePreferences,
} from '#core/infrastructure/preferences/compareWorkspacePreferences.ts';
import { createCompareWorkspace } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import { deriveWorkspaceExecutionAccess } from '#core/application/compare-workspace/compareWorkspaceExecution.ts';
import { denyingStorage, memoryStorage } from './preferenceStorageDouble.mts';

function operation(path: string, action: PlanOperation['action'] = 'copy'): PlanOperation {
  return { side: 'target', action, path, reason: 'fixture' };
}

function plan(operations: PlanOperation[]): PlanDto {
  return {
    owner: {} as PlanDto['owner'],
    header: { generated_at_ms: Date.now() } as PlanDto['header'],
    ops: operations,
    metas: operations.map(() => null),
    identical_count: 0,
    identical_bytes: 0,
    mtime_window_ms: 2000,
  };
}

function inScope(result: PlanDto, folderScope: string | null): number[] {
  return computeInScopeIndices({
    plan: result,
    reversedRows: [],
    selectedResultTypes: new Set(),
    searchQuery: '',
    folderScope,
    advancedFilter: EMPTY_ADVANCED_SCOPE_FILTER,
    excludedByMask: [],
  });
}

test('every result type is independently represented', () => {
  assert.deepEqual(RESULT_TYPES, ['copy', 'update', 'move', 'delete', 'conflict', 'note']);
  assert.equal(resultTypeOf(operation('copy.txt')), 'copy');
  assert.equal(resultTypeOf(operation('update.txt', 'update')), 'update');
  assert.equal(resultTypeOf(operation('mode.txt', 'chmod')), 'update');
  assert.equal(resultTypeOf(operation('move.txt', 'move')), 'move');
  assert.equal(resultTypeOf(operation('delete.txt', 'delete')), 'delete');
  assert.equal(resultTypeOf(operation('folder', 'delete_dir')), 'delete');
  assert.equal(resultTypeOf(operation('conflict.txt', 'conflict')), 'conflict');
  assert.equal(resultTypeOf(operation('note.txt', 'note')), 'note');
  assert.equal(describeRowAction(operation('note.txt', 'note')).resultType, 'note');

  const deleted = operation('delete.txt', 'delete');
  deleted.hash = 'stale-delete-evidence';
  const reversible = plan([operation('copy.txt'), deleted]);
  assert.equal(resultTypeOf(effectiveOperation(reversible, [true, false], 0)), 'delete');
  const restored = effectiveOperation(reversible, [false, true], 1);
  assert.equal(resultTypeOf(restored), 'copy');
  assert.equal(restored.hash, null);
});

test('root folder scope is distinct from no scope and from a folder literally named root', () => {
  assert.equal(matchesFolderScope(operation('root-file.txt'), null), true);
  assert.equal(matchesFolderScope(operation('root-file.txt'), ''), true);
  assert.equal(matchesFolderScope(operation('top-folder', 'delete_dir'), ''), false);
  assert.equal(matchesFolderScope(operation('(root)/inside.txt'), ''), false);
  assert.equal(matchesFolderScope(operation('(root)/inside.txt'), '(root)'), true);

  const result = plan([
    operation('root-file.txt'),
    operation('(root)/inside.txt'),
    operation('other/deep.txt'),
  ]);
  assert.deepEqual(inScope(result, null), [0, 1, 2]);
  assert.deepEqual(inScope(result, ''), [0]);
  assert.deepEqual(inScope(result, '(root)'), [1]);
});

test('folder scope uses the same ownership rule as the folder hierarchy', () => {
  const result = plan([
    operation('replace-me'),
    operation('replace-me', 'delete_dir'),
    operation('replace-me/child.txt'),
  ]);
  assert.deepEqual(inScope(result, ''), [0]);
  assert.deepEqual(inScope(result, 'replace-me'), [1, 2]);
});

test('executable indices preserve engine order and reject report rows', () => {
  const result = plan([
    operation('first.txt'),
    operation('report.txt', 'note'),
    operation('last.txt', 'delete'),
  ]);
  assert.deepEqual(computeExecutableIndices(result, [], [0, 1, 2], [true, true, true]), [0, 2]);
});

test('mask and result-type criteria narrow the run scope together', () => {
  const result = plan([
    operation('copy.txt'),
    operation('delete.txt', 'delete'),
    operation('note.txt', 'note'),
  ]);
  const indices = computeInScopeIndices({
    plan: result,
    reversedRows: [],
    selectedResultTypes: new Set<ResultType>(['delete', 'note']),
    searchQuery: '.txt',
    folderScope: null,
    advancedFilter: EMPTY_ADVANCED_SCOPE_FILTER,
    excludedByMask: [false, true, false],
  });
  assert.deepEqual(indices, [2]);
});

test('scope masks normalize one shared draft format', () => {
  assert.deepEqual(parseScopeMasks('  */*.log\n\n/docs/  \n'), ['*/*.log', '/docs/']);
  assert.equal(isMaskMatchResult([true, false], 2), true);
  assert.equal(isMaskMatchResult([true], 2), false);
  assert.equal(isMaskMatchResult([true, 'false'], 2), false);
});

test('a first load seeds the defaults and writes one coherent preference set', () => {
  const storage = memoryStorage();
  const loaded = loadCompareWorkspacePreferences(storage);
  const preferences = loaded.preferences;
  assert.equal(loaded.warning, null);
  assert.deepEqual(preferences, { grouped: true, pathMode: 'relative', scopePanelCollapsed: true });
  assert.deepEqual(
    JSON.parse(storage.values.get('sd.compare-workspace-preferences.v1')!),
    preferences,
  );
  assert.equal(saveCompareWorkspacePreferences(storage, {
    ...preferences,
    scopePanelCollapsed: true,
    grouped: false,
    pathMode: 'full',
  }), null);
  assert.deepEqual(JSON.parse(storage.values.get('sd.compare-workspace-preferences.v1')!), {
    scopePanelCollapsed: true,
    grouped: false,
    pathMode: 'full',
  });
});

test('compare workspace preference storage failures return defaults and diagnostics', () => {
  const storage = denyingStorage('storage denied');
  const loaded = loadCompareWorkspacePreferences(storage);
  assert.equal(loaded.preferences.pathMode, 'relative');
  assert.match(loaded.warning ?? '', /storage denied/);
  assert.match(saveCompareWorkspacePreferences(storage, loaded.preferences) ?? '', /storage denied/);
});

test('size and modified-time criteria use immutable compare evidence', () => {
  const now = Date.now();
  const smallRecent = operation('small-recent.txt');
  const largeOld = operation('large-old.txt', 'update');
  const result = plan([smallRecent, largeOld]);
  result.header.generated_at_ms = now;
  result.metas = [
    { src: { size: 2 * 1024 * 1024, mtime_ms: now }, dst: null },
    {
      src: { size: 20 * 1024 * 1024, mtime_ms: now - 40 * 86_400_000 },
      dst: { size: 18 * 1024 * 1024, mtime_ms: now - 40 * 86_400_000 },
    },
  ];

  assert.deepEqual(computeInScopeIndices({
    plan: result,
    reversedRows: [],
    selectedResultTypes: new Set(),
    searchQuery: '',
    folderScope: null,
    advancedFilter: {
      masks: [],
      minimumMiB: 1,
      maximumMiB: 10,
      modifiedWithinDays: 7,
    },
    excludedByMask: [],
  }), [0]);
});

test('run-scope model has one exhaustive result vocabulary and root-level sibling', () => {
  const deletion = operation('docs/archive/delete.txt', 'delete');
  deletion.size = 4096;
  const result = plan([
    operation('root.txt'),
    operation('docs/copy.txt'),
    deletion,
    operation('report.txt', 'conflict'),
  ]);
  const model = buildRunScopeModel(result, []);
  assert.deepEqual(model.folders.map((folder) => folder.path), ['', 'docs']);
  assert.deepEqual(model.countByResultType, {
    copy: 2,
    update: 0,
    move: 0,
    delete: 1,
    conflict: 1,
    note: 0,
  });
  const docs = model.folders.find((folder) => folder.path === 'docs');
  assert.equal(docs?.deletionBytes, 4096);
  assert.equal(docs?.children.find((folder) => folder.path === 'docs/archive')?.deletionBytes, 4096);
});

test('reversed updates count bytes from their new origin side', () => {
  const update = operation('changed.txt', 'update');
  update.size = 100;
  const result = plan([update]);
  result.metas = [{
    src: { size: 100, mtime_ms: 2 },
    dst: { size: 40, mtime_ms: 1 },
  }];

  assert.equal(rowTransferBytes(result, [false], 0), 100);
  assert.equal(rowTransferBytes(result, [true], 0), 40);
});

test('apply availability is one fail-closed guard for the active result workspace', () => {
  const resultPlan = plan([operation('run.txt')]);
  resultPlan.owner = {
    identity: {
      result_id: '44444444444444444444444444444444',
      compare_run_id: 9,
      job_id: 'job-id',
      target_index: 0,
      config_revision: 'revision',
    },
    job_name: 'Job',
  };
  const workspace = createCompareWorkspace(resultPlan);
  const executable = deriveWorkspaceExecutionAccess(workspace, {
    status: 'fresh',
    scope: { job_id: 'job-id', target_index: 0, config_revision: 'revision' },
    attempt: { verification_epoch: 4, compare_run_id: 9 },
    owner: resultPlan.owner,
  });
  assert.match(deriveApplyAvailability({
    workspace: null,
    workspaceExecutionAccess: null,
    compareActivity: null,
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 0,
  }).blockedMessage ?? '', /Compare first/);
  assert.match(deriveApplyAvailability({
    workspace,
    workspaceExecutionAccess: deriveWorkspaceExecutionAccess(workspace, null),
    compareActivity: null,
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 1,
  }).blockedMessage ?? '', /execution authority/);
  assert.equal(deriveApplyAvailability({
    workspace: { ...workspace, selectedView: 'identical' },
    workspaceExecutionAccess: executable,
    compareActivity: null,
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 3,
  }).available, false);
  assert.match(deriveApplyAvailability({
    workspace,
    workspaceExecutionAccess: executable,
    compareActivity: null,
    scopeCalculationPending: false,
    scopeCalculationFailed: true,
    executableCount: 3,
  }).blockedMessage ?? '', /could not be calculated safely/);
  assert.match(deriveApplyAvailability({
    workspace,
    workspaceExecutionAccess: executable,
    compareActivity: null,
    scopeCalculationPending: true,
    scopeCalculationFailed: false,
    executableCount: 3,
  }).blockedMessage ?? '', /still being calculated/);
  assert.match(deriveApplyAvailability({
    workspace,
    workspaceExecutionAccess: executable,
    compareActivity: null,
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 0,
  }).blockedMessage ?? '', /No included differences/);
  assert.equal(deriveApplyAvailability({
    workspace,
    workspaceExecutionAccess: executable,
    compareActivity: null,
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 3,
  }).available, true);
  assert.match(deriveApplyAvailability({
    workspace,
    workspaceExecutionAccess: executable,
    compareActivity: {
      status: 'comparing',
      requestId: 1,
      origin: { kind: 'auto_scan', generation: 2, ticketId: 3 },
    },
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 3,
  }).blockedMessage ?? '', /newer Compare attempt/);
});
