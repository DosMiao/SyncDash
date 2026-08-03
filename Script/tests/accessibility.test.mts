import assert from 'node:assert/strict';
import test from 'node:test';

import { isRovingFocusKey, rovingFocusIndex } from '#ui/shared/components/a11y.ts';

test('roving arrow navigation wraps in both directions', () => {
  assert.equal(rovingFocusIndex('ArrowDown', 2, 3), 0);
  assert.equal(rovingFocusIndex('ArrowUp', 0, 3), 2);
  assert.equal(rovingFocusIndex('ArrowDown', -1, 3), 0);
  assert.equal(rovingFocusIndex('ArrowUp', -1, 3), 2);
});

test('the horizontal axis walks the same ring as the vertical axis', () => {
  assert.equal(rovingFocusIndex('ArrowRight', 2, 3), rovingFocusIndex('ArrowDown', 2, 3));
  assert.equal(rovingFocusIndex('ArrowLeft', 0, 3), rovingFocusIndex('ArrowUp', 0, 3));
  assert.equal(rovingFocusIndex('ArrowRight', -1, 3), 0);
  assert.equal(rovingFocusIndex('ArrowLeft', -1, 3), 2);
});

test('roving boundary keys and empty lists are deterministic', () => {
  assert.equal(rovingFocusIndex('Home', 2, 4), 0);
  assert.equal(rovingFocusIndex('End', 0, 4), 3);
  assert.equal(rovingFocusIndex('ArrowDown', -1, 0), null);
  assert.equal(rovingFocusIndex('End', 0, 0), null);
});

test('only navigation keys are claimed by a roving-focus widget', () => {
  for (const key of ['ArrowDown', 'ArrowRight', 'ArrowUp', 'ArrowLeft', 'Home', 'End']) {
    assert.equal(isRovingFocusKey(key), true, key);
  }
  for (const key of ['Enter', ' ', 'Tab', 'Escape', 'PageDown', 'a']) {
    assert.equal(isRovingFocusKey(key), false, key);
  }
});
