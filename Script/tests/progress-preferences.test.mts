import assert from 'node:assert/strict';
import test from 'node:test';

import {
  loadProgressPreferences,
  saveAutoClosePreference,
  saveWhenFinishedPreference,
  WHEN_FINISHED_PREFERENCE_KEY,
} from '#core/infrastructure/preferences/progressPreferences.ts';
import { denyingStorage, memoryStorage } from './preferenceStorageDouble.mts';

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

  const failed = loadProgressPreferences(denyingStorage('storage denied'));
  assert.equal(failed.autoCloseEnabled, false);
  assert.equal(failed.whenFinishedAction, 'none');
  assert.equal(failed.failures.length, 2);
});

test('progress preference writes are typed and return storage failures', () => {
  const storage = memoryStorage();
  assert.equal(saveAutoClosePreference(storage, true), null);
  assert.equal(saveWhenFinishedPreference(storage, 'shutdown'), null);
  assert.deepEqual(storage.writes, [
    ['sd.autoclose', '1'],
    [WHEN_FINISHED_PREFERENCE_KEY, 'shutdown'],
  ]);

  const failure = denyingStorage('storage denied');
  assert.match(saveAutoClosePreference(failure, false) ?? '', /storage denied/);
  assert.match(saveWhenFinishedPreference(failure, 'none') ?? '', /storage denied/);
});
