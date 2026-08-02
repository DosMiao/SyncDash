import assert from 'node:assert/strict';
import test from 'node:test';

import {
  addPathToHistory,
  loadPathHistory,
  savePathHistory,
} from '#core/infrastructure/preferences/pathHistory.ts';

test('path history validates, normalizes, deduplicates, and bounds persisted input', () => {
  const values = new Map<string, string>([[
    'sd.pathhist',
    JSON.stringify([' /One ', '/one', '', ...Array.from({ length: 20 }, (_, index) => `/p${index}`)]),
  ]]);
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  };
  const loaded = loadPathHistory(storage);
  assert.equal(loaded.warning, null);
  assert.equal(loaded.paths.length, 12);
  assert.equal(loaded.paths[0], '/One');
  assert.deepEqual(JSON.parse(values.get('sd.path-history.v1')!), loaded.paths);
  assert.equal(values.has('sd.pathhist'), false);
});

test('path history updates are stable and storage failures are visible', () => {
  assert.deepEqual(addPathToHistory(['/a', '/b'], ' /B '), ['/B', '/a']);
  const denied = {
    getItem: () => { throw new Error('denied'); },
    setItem: () => { throw new Error('denied'); },
    removeItem: () => { throw new Error('denied'); },
  };
  assert.match(loadPathHistory(denied).warning ?? '', /denied/);
  assert.match(savePathHistory(denied, ['/a']) ?? '', /denied/);
});
