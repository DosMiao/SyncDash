import assert from 'node:assert/strict';
import test from 'node:test';

import { menuFocusIndex } from '../../typescript/ui/components/a11y.ts';

test('menu arrow navigation wraps in both directions', () => {
  assert.equal(menuFocusIndex('ArrowDown', 2, 3), 0);
  assert.equal(menuFocusIndex('ArrowUp', 0, 3), 2);
  assert.equal(menuFocusIndex('ArrowDown', -1, 3), 0);
  assert.equal(menuFocusIndex('ArrowUp', -1, 3), 2);
});

test('menu boundary keys and empty menus are deterministic', () => {
  assert.equal(menuFocusIndex('Home', 2, 4), 0);
  assert.equal(menuFocusIndex('End', 0, 4), 3);
  assert.equal(menuFocusIndex('ArrowDown', -1, 0), null);
});
