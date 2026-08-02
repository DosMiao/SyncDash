import assert from 'node:assert/strict';
import test from 'node:test';

import {
  compareLaunchBlocked,
  interactionBlocksUnattendedWrite,
  interactionConflictsWithReservedWrite,
  rootEditKeyAction,
  rootSaveBlocked,
} from '#core/application/safety/executionSafety.ts';
import type { ExecutionInteractionState } from '#core/application/safety/executionSafety.ts';

test('Escape reverts a root edit while Enter is the only commit key', () => {
  assert.equal(rootEditKeyAction('Escape'), 'revert');
  assert.equal(rootEditKeyAction('Enter'), 'commit');
  assert.equal(rootEditKeyAction('Tab'), null);
});

test('every live interaction owner blocks an unattended write', () => {
  const idle: ExecutionInteractionState = {
    busy: false,
    editorOpen: false,
    settingsOpen: false,
    confirmationOpen: false,
    candidateAdoptionOpen: false,
    rootDraftOpen: false,
    rootSwapOpen: false,
    contextMenuOpen: false,
    reviewPending: false,
  };
  assert.equal(interactionBlocksUnattendedWrite(idle), false);
  assert.equal(interactionConflictsWithReservedWrite(idle), false);
  for (const key of Object.keys(idle) as Array<keyof ExecutionInteractionState>) {
    assert.equal(
      interactionBlocksUnattendedWrite({ ...idle, [key]: true }),
      true,
      `${key} must own execution while it is active`,
    );
  }
  assert.equal(interactionConflictsWithReservedWrite({ ...idle, busy: true }), false);
  for (const key of Object.keys(idle) as Array<keyof ExecutionInteractionState>) {
    if (key === 'busy') continue;
    assert.equal(
      interactionConflictsWithReservedWrite({ ...idle, [key]: true }),
      true,
      `${key} must invalidate a reserved unattended write`,
    );
  }
});

const idleInteraction: ExecutionInteractionState = {
  busy: false,
  editorOpen: false,
  settingsOpen: false,
  confirmationOpen: false,
  candidateAdoptionOpen: false,
  rootDraftOpen: false,
  rootSwapOpen: false,
  contextMenuOpen: false,
  reviewPending: false,
};

const owners = [
  'busy',
  'editorOpen',
  'settingsOpen',
  'confirmationOpen',
  'candidateAdoptionOpen',
  'rootDraftOpen',
  'rootSwapOpen',
  'contextMenuOpen',
  'reviewPending',
] as const;

/// The two execution gates deliberately differ from the full ownership set, each by exactly one
/// field, and each omission is required for its own path to work at all. They were previously
/// hand-written at their call sites, where the difference read as drift; pinning it here is what
/// stops a later "make these consistent" edit from deadlocking one of them.
test('the Compare gate omits only reviewPending, because a review authorizes the Compare it blocks', () => {
  assert.equal(compareLaunchBlocked(idleInteraction), false);

  for (const owner of owners) {
    const blocked = compareLaunchBlocked({ ...idleInteraction, [owner]: true });
    if (owner === 'reviewPending') {
      assert.equal(blocked, false, 'a pending review must not block the Compare it authorized');
    } else {
      assert.equal(blocked, true, `${owner} must block an authorized Compare`);
    }
  }
});

test('the root-save gate omits only rootDraftOpen, because the draft being saved is that draft', () => {
  assert.equal(rootSaveBlocked(idleInteraction), false);

  for (const owner of owners) {
    const blocked = rootSaveBlocked({ ...idleInteraction, [owner]: true });
    if (owner === 'rootDraftOpen') {
      assert.equal(blocked, false, 'an open root draft must not block saving that draft');
    } else {
      assert.equal(blocked, true, `${owner} must block a root save`);
    }
  }
});
