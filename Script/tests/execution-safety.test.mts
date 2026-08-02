import assert from 'node:assert/strict';
import test from 'node:test';

import {
  interactionBlocksUnattendedWrite,
  interactionConflictsWithReservedWrite,
  rootEditKeyAction,
} from '../../typescript/ui/state/execution-safety.ts';
import type { ExecutionInteractionState } from '../../typescript/ui/state/execution-safety.ts';

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
