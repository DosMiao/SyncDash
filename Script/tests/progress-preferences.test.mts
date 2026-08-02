import assert from 'node:assert/strict';
import test from 'node:test';

import {
  loadProgressPreferences,
  saveAutoClosePreference,
  saveWhenFinishedPreference,
  WHEN_FINISHED_PREFERENCE_KEY,
} from '#core/infrastructure/preferences/progressPreferences.ts';

function memoryStorage(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  const writes: Array<[string, string]> = [];
  const removals: string[] = [];
  return {
    values,
    writes,
    removals,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      writes.push([key, value]);
      values.set(key, value);
    },
    removeItem: (key: string) => {
      removals.push(key);
      values.delete(key);
    },
  };
}

test('progress preferences validate stored tokens and report read failures', () => {
  const validStorage = memoryStorage({
    'sd.autoclose': '1',
    [WHEN_FINISHED_PREFERENCE_KEY]: 'sleep',
  });
  const valid = loadProgressPreferences(validStorage);
  assert.deepEqual(valid, { autoCloseEnabled: true, whenFinishedAction: 'sleep', failures: [] });

  const invalidStorage = memoryStorage({
    'sd.autoclose': 'yes',
    [WHEN_FINISHED_PREFERENCE_KEY]: 'hibernate',
  });
  const invalid = loadProgressPreferences(invalidStorage);
  assert.equal(invalid.autoCloseEnabled, false);
  assert.equal(invalid.whenFinishedAction, 'none');
  assert.deepEqual(invalid.failures, [
    'Auto-close preference is invalid and was ignored',
    'When-finished preference is invalid and was ignored',
  ]);

  const failed = loadProgressPreferences({
    getItem: () => { throw new Error('storage denied'); },
    setItem: () => { throw new Error('unexpected write'); },
    removeItem: () => { throw new Error('unexpected removal'); },
  });
  assert.equal(failed.autoCloseEnabled, false);
  assert.equal(failed.whenFinishedAction, 'none');
  assert.equal(failed.failures.length, 2);
});

test('legacy When-finished preference is preserved through the versioned-key migration', () => {
  const storage = memoryStorage({ 'sd.whenfin': 'shutdown' });
  const loaded = loadProgressPreferences(storage);
  assert.equal(loaded.whenFinishedAction, 'shutdown');
  assert.deepEqual(loaded.failures, []);
  assert.equal(storage.values.get(WHEN_FINISHED_PREFERENCE_KEY), 'shutdown');
  assert.equal(storage.values.has('sd.whenfin'), false);
});

test('progress preference writes are typed and return storage failures', () => {
  const writes = new Map<string, string>();
  const storage = { setItem: (key: string, value: string) => { writes.set(key, value); } };
  assert.equal(saveAutoClosePreference(storage, true), null);
  assert.equal(saveWhenFinishedPreference(storage, 'shutdown'), null);
  assert.deepEqual([...writes], [
    ['sd.autoclose', '1'],
    [WHEN_FINISHED_PREFERENCE_KEY, 'shutdown'],
  ]);

  const failure = { setItem: () => { throw new Error('storage denied'); } };
  assert.match(saveAutoClosePreference(failure, false) ?? '', /storage denied/);
  assert.match(saveWhenFinishedPreference(failure, 'none') ?? '', /storage denied/);
});
