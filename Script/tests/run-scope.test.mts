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
} from '../../typescript/core/runScope.ts';
import {
  RESULT_TYPES,
  resultTypeOf,
  describeRowAction,
  effectiveOperation,
  rowTransferBytes,
} from '../../typescript/core/plan.ts';
import type { PlanDto, PlanOperation, ResultType } from '../../typescript/core/plan.ts';
import {
  deriveApplyAvailability,
  identicalResultRequestKey,
  readRunScopePanelCollapsed,
  writeRunScopePanelCollapsed,
} from '../../typescript/ui/state/result-workspace.ts';
import type { CompareOwner } from '../../typescript/core/types/generated/CompareOwner.ts';

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
  };
}

function inScope(result: PlanDto, folderScope: string | null): number[] {
  return computeInScopeIndices({
    plan: result,
    flipped: [],
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
    flipped: [],
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

test('run-scope panel preference migrates once and writes only the current key', () => {
  const values = new Map<string, string>([['sd.ov', 'open']]);
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  };
  assert.equal(readRunScopePanelCollapsed(storage), false);
  assert.equal(values.get('sd.scope'), 'open');
  assert.equal(values.has('sd.ov'), false);
  writeRunScopePanelCollapsed(storage, true);
  assert.equal(values.get('sd.scope'), 'closed');
});

test('identical request identity fences every provenance dimension, query, and page', () => {
  const owner: CompareOwner = {
    identity: {
      compare_run_id: 7,
      job_id: 'job-id',
      target_index: 2,
      config_revision: 'revision',
    },
    job_name: 'Display Name',
  };
  const baseline = identicalResultRequestKey(owner, 'docs', 300);
  assert.notEqual(identicalResultRequestKey({
    ...owner,
    identity: { ...owner.identity, compare_run_id: 8 },
  }, 'docs', 300), baseline);
  assert.notEqual(identicalResultRequestKey({
    ...owner,
    identity: { ...owner.identity, job_id: 'other' },
  }, 'docs', 300), baseline);
  assert.notEqual(identicalResultRequestKey({
    ...owner,
    identity: { ...owner.identity, target_index: 3 },
  }, 'docs', 300), baseline);
  assert.notEqual(identicalResultRequestKey({
    ...owner,
    identity: { ...owner.identity, config_revision: 'new' },
  }, 'docs', 300), baseline);
  assert.notEqual(identicalResultRequestKey(owner, 'src', 300), baseline);
  assert.notEqual(identicalResultRequestKey(owner, 'docs', 600), baseline);
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
    flipped: [],
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
  assert.match(deriveApplyAvailability({
    hasPlan: false,
    resultView: 'differences',
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 0,
  }).blockedMessage ?? '', /Compare first/);
  assert.equal(deriveApplyAvailability({
    hasPlan: true,
    resultView: 'identical',
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 3,
  }).available, false);
  assert.match(deriveApplyAvailability({
    hasPlan: true,
    resultView: 'differences',
    scopeCalculationPending: false,
    scopeCalculationFailed: true,
    executableCount: 3,
  }).blockedMessage ?? '', /could not be calculated safely/);
  assert.match(deriveApplyAvailability({
    hasPlan: true,
    resultView: 'differences',
    scopeCalculationPending: true,
    scopeCalculationFailed: false,
    executableCount: 3,
  }).blockedMessage ?? '', /still being calculated/);
  assert.match(deriveApplyAvailability({
    hasPlan: true,
    resultView: 'differences',
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 0,
  }).blockedMessage ?? '', /No checked differences/);
  assert.equal(deriveApplyAvailability({
    hasPlan: true,
    resultView: 'differences',
    scopeCalculationPending: false,
    scopeCalculationFailed: false,
    executableCount: 3,
  }).available, true);
});
