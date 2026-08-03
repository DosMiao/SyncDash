import assert from 'node:assert/strict';
import test from 'node:test';

import {
  addPathToHistory,
  loadPathHistory,
  savePathHistory,
} from '#core/infrastructure/preferences/pathHistory.ts';
import { denyingStorage, memoryStorage } from './preferenceStorageDouble.mts';

test('path history validates, normalizes, deduplicates, and bounds persisted input', () => {
  const storage = memoryStorage({
    'sd.path-history.v1': JSON.stringify([
      ' /One ',
      '/one',
      '',
      ...Array.from({ length: 20 }, (_, index) => `/p${index}`),
    ]),
  });
  const loaded = loadPathHistory(storage);
  assert.equal(loaded.warning, null);
  assert.equal(loaded.paths.length, 12);
  assert.equal(loaded.paths[0], '/One');
});

test('path history updates are stable and storage failures are visible', () => {
  assert.deepEqual(addPathToHistory(['/a', '/b'], ' /B '), ['/B', '/a']);
  const denied = denyingStorage('denied');
  assert.match(loadPathHistory(denied).warning ?? '', /denied/);
  assert.match(savePathHistory(denied, ['/a']) ?? '', /denied/);
});
