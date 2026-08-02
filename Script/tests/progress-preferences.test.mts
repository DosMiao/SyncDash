import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

import {
  loadProgressPreferences,
  saveAutoClosePreference,
  saveWhenFinishedPreference,
  WHEN_FINISHED_PREFERENCE_KEY,
} from '../../typescript/progress/preferences.ts';

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
    'sd.whenfin': 'shutdown',
  });
  const invalid = loadProgressPreferences(invalidStorage);
  assert.equal(invalid.autoCloseEnabled, false);
  assert.equal(invalid.whenFinishedAction, 'none');
  assert.deepEqual(invalid.failures, [
    'Auto-close preference is invalid and was ignored',
    'When-finished preference is invalid and was ignored',
  ]);
  assert.equal(invalidStorage.values.get('sd.whenfin'), 'shutdown');

  const failed = loadProgressPreferences({
    getItem: () => { throw new Error('storage denied'); },
    setItem: () => { throw new Error('unexpected write'); },
    removeItem: () => { throw new Error('unexpected removal'); },
  });
  assert.equal(failed.autoCloseEnabled, false);
  assert.equal(failed.whenFinishedAction, 'none');
  assert.equal(failed.failures.length, 2);
});

test('the legacy When-finished key migrates once and the versioned key is authoritative', () => {
  const legacyStorage = memoryStorage({ 'sd.whenfin': 'shutdown' });
  const migrated = loadProgressPreferences(legacyStorage);

  assert.deepEqual(migrated, {
    autoCloseEnabled: false,
    whenFinishedAction: 'shutdown',
    failures: [],
  });
  assert.deepEqual(legacyStorage.writes, [[WHEN_FINISHED_PREFERENCE_KEY, 'shutdown']]);
  assert.deepEqual(legacyStorage.removals, ['sd.whenfin']);
  assert.equal(legacyStorage.values.get(WHEN_FINISHED_PREFERENCE_KEY), 'shutdown');
  assert.equal(legacyStorage.values.has('sd.whenfin'), false);

  const versionedStorage = memoryStorage({
    [WHEN_FINISHED_PREFERENCE_KEY]: 'sleep',
    'sd.whenfin': 'shutdown',
  });
  const authoritative = loadProgressPreferences(versionedStorage);
  assert.equal(authoritative.whenFinishedAction, 'sleep');
  assert.deepEqual(versionedStorage.writes, []);
  assert.deepEqual(versionedStorage.removals, ['sd.whenfin']);
});

test('a failed migration keeps the legacy value and reports that persistence failed', () => {
  let legacyRemoved = false;
  const result = loadProgressPreferences({
    getItem: (key: string) => (key === 'sd.whenfin' ? 'sleep' : null),
    setItem: () => { throw new Error('storage denied'); },
    removeItem: () => { legacyRemoved = true; },
  });

  assert.equal(result.whenFinishedAction, 'sleep');
  assert.match(result.failures[0] ?? '', /Could not migrate.*storage denied/);
  assert.equal(legacyRemoved, false);
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

test('progress error disclosure and controls retain native button semantics', async () => {
  const source = await readFile(new URL('../../typescript/progress/ProgressApp.tsx', import.meta.url), 'utf8');
  const buttonTags = [...source.matchAll(/<button\b[\s\S]*?>/g)].map((match) => match[0]);
  assert.ok(buttonTags.length >= 6);
  assert.ok(buttonTags.every((tag) => /\btype="button"/.test(tag)));
  assert.match(source, /<button[\s\S]*?className="errhead"[\s\S]*?aria-expanded=\{errorDetailsOpen\}[\s\S]*?aria-controls="progress-errors"/);
  assert.match(source, /id="progress-errors"\s+className="errlist"/);
  assert.doesNotMatch(source, /<div\s+className="errhead"/);
});
