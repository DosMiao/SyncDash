import assert from 'node:assert/strict';
import test from 'node:test';

import {
  activeRootEditor,
  emptyRootEditorRepository,
  reduceRootEditors,
  rootDraftIsDirty,
} from '#core/application/jobs/rootEditor.ts';
import type { RootEditorOwner } from '#core/application/jobs/rootEditor.ts';

const owner: RootEditorOwner = {
  jobId: 'job-a',
  jobName: 'Photos',
  configRevision: 'revision-a',
  targetIndex: 1,
};

function select(
  repository = emptyRootEditorRepository,
  selectedOwner = owner,
  source = '/source',
  target = '/target',
) {
  return reduceRootEditors(repository, {
    type: 'selection_rebound',
    owner: selectedOwner,
    values: { source, target },
  });
}

test('root drafts are retained independently per job target', () => {
  let repository = select();
  const photosKey = activeRootEditor(repository)!.key;
  repository = reduceRootEditors(repository, {
    type: 'draft_changed', workspaceKey: photosKey, field: 'target', value: '/draft',
  });
  repository = select(
    repository,
    { ...owner, jobId: 'job-b', jobName: 'Archive', targetIndex: 0 },
    '/archive-source',
    '/archive-target',
  );
  assert.equal(activeRootEditor(repository)?.draft.target, '/archive-target');
  repository = select(repository);
  assert.equal(activeRootEditor(repository)?.draft.target, '/draft');
});

test('same-selection refresh updates clean fields and exposes dirty-field conflicts', () => {
  let repository = select();
  const workspaceKey = activeRootEditor(repository)!.key;
  repository = reduceRootEditors(repository, {
    type: 'draft_changed', workspaceKey, field: 'target', value: '/draft',
  });
  repository = select(
    repository,
    { ...owner, jobName: 'Renamed', configRevision: 'revision-b' },
    '/new-source',
    '/external-target',
  );
  const workspace = activeRootEditor(repository)!;
  assert.equal(workspace.owner.jobName, 'Renamed');
  assert.equal(workspace.draft.source, '/new-source');
  assert.equal(workspace.draft.target, '/draft');
  assert.deepEqual(workspace.conflicts.target, {
    previousCommittedValue: '/target',
    currentCommittedValue: '/external-target',
  });
});

test('root save transitions are exact-request fenced and preserve a failed draft', () => {
  let repository = select();
  const workspaceKey = activeRootEditor(repository)!.key;
  repository = reduceRootEditors(repository, {
    type: 'draft_changed', workspaceKey, field: 'source', value: ' /changed ',
  });
  assert.equal(rootDraftIsDirty(activeRootEditor(repository)!, 'source'), true);
  repository = reduceRootEditors(repository, {
    type: 'save_started', workspaceKey, requestId: 4, field: 'source',
  });
  assert.deepEqual(activeRootEditor(repository)?.save, {
    status: 'saving',
    requestId: 4,
    field: 'source',
  });
  const unchanged = reduceRootEditors(repository, {
    type: 'draft_changed', workspaceKey, field: 'source', value: '/race',
  });
  assert.equal(unchanged, repository);
  assert.equal(reduceRootEditors(repository, {
    type: 'save_failed', workspaceKey, requestId: 3, error: 'stale',
  }), repository);
  repository = reduceRootEditors(repository, {
    type: 'save_failed', workspaceKey, requestId: 4, error: 'conflict',
  });
  assert.equal(activeRootEditor(repository)?.save.status, 'failed');
  assert.equal(activeRootEditor(repository)?.draft.source, ' /changed ');
});

test('conflicts require an explicit keep-draft decision before saving', () => {
  let repository = select();
  const workspaceKey = activeRootEditor(repository)!.key;
  repository = reduceRootEditors(repository, {
    type: 'draft_changed', workspaceKey, field: 'source', value: '/draft',
  });
  repository = select(
    repository,
    { ...owner, configRevision: 'revision-b' },
    '/external',
    '/target',
  );
  repository = reduceRootEditors(repository, {
    type: 'save_started', workspaceKey, requestId: 1, field: 'source',
  });
  assert.equal(activeRootEditor(repository)?.save.status, 'idle');
  repository = reduceRootEditors(repository, {
    type: 'draft_conflict_accepted', workspaceKey, field: 'source',
  });
  repository = reduceRootEditors(repository, {
    type: 'save_started', workspaceKey, requestId: 1, field: 'source',
  });
  assert.equal(activeRootEditor(repository)?.save.status, 'saving');
});

test('explicit cancel reverts one field without mutating the other draft', () => {
  let repository = select();
  const workspaceKey = activeRootEditor(repository)!.key;
  repository = reduceRootEditors(repository, {
    type: 'draft_changed', workspaceKey, field: 'source', value: '/source-draft',
  });
  repository = reduceRootEditors(repository, {
    type: 'draft_changed', workspaceKey, field: 'target', value: '/target-draft',
  });
  repository = reduceRootEditors(repository, {
    type: 'draft_reverted', workspaceKey, field: 'source',
  });
  assert.deepEqual(activeRootEditor(repository)?.draft, {
    source: '/source', target: '/target-draft',
  });
});
