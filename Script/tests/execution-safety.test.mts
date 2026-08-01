import assert from 'node:assert/strict';
import test from 'node:test';

import {
  rootEditKeyAction,
} from '../../typescript/ui/state/execution-safety.ts';

test('Escape reverts a root edit while Enter is the only commit key', () => {
  assert.equal(rootEditKeyAction('Escape'), 'revert');
  assert.equal(rootEditKeyAction('Enter'), 'commit');
  assert.equal(rootEditKeyAction('Tab'), null);
});
