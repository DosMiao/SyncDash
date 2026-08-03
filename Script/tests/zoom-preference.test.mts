import assert from 'node:assert/strict';
import test from 'node:test';

import {
  ZoomAuthority,
  createInitialZoomAuthorityState,
  stepZoom,
} from '#core/application/zoom/zoomAuthority.ts';
import type {
  ZoomAuthorityFailure,
  ZoomAuthorityState,
  ZoomFactor,
} from '#core/application/zoom/zoomAuthority.ts';
import {
  loadZoomPreference,
  saveZoomPreference,
} from '#core/infrastructure/preferences/zoomPreference.ts';
import { deferred, flushMicrotasks, type Deferred } from './asyncSettlement.mts';
import { denyingStorage, memoryStorage } from './preferenceStorageDouble.mts';

function initialAuthorityState(): ZoomAuthorityState {
  return createInitialZoomAuthorityState({
    factor: 1,
    persistedFactor: 1,
    warning: null,
  });
}

test('zoom preference accepts only supported discrete factors', () => {
  assert.deepEqual(loadZoomPreference(memoryStorage()), {
    factor: 1,
    persistedFactor: null,
    warning: null,
  });
  assert.deepEqual(loadZoomPreference(memoryStorage({ 'sd.zoom': '1.25' })), {
    factor: 1.25,
    persistedFactor: 1.25,
    warning: null,
  });
  const invalid = loadZoomPreference(memoryStorage({ 'sd.zoom': '1.234' }));
  assert.equal(invalid.factor, 1);
  assert.equal(invalid.persistedFactor, null);
  assert.match(invalid.warning ?? '', /invalid interface zoom/i);
});

test('zoom storage failures are explicit and stepping rejects unknown state', () => {
  const loaded = loadZoomPreference(denyingStorage('read denied'));
  assert.equal(loaded.factor, 1);
  assert.equal(loaded.persistedFactor, null);
  assert.match(loaded.warning ?? '', /read denied/);

  const failingWrite = denyingStorage('write denied');
  assert.match(saveZoomPreference(1.25, failingWrite) ?? '', /write denied/);
  assert.throws(() => saveZoomPreference(1.234, failingWrite), /Invalid interface zoom factor/);

  assert.equal(stepZoom(0.8, -1), 0.8);
  assert.equal(stepZoom(2, 1), 2);
  assert.throws(() => stepZoom(1.234, 1), /Invalid interface zoom factor/);
});

test('rapid requests coalesce behind a monotonic fence and persist only the latest applied factor', async () => {
  const applications: Array<{ factor: ZoomFactor; completion: Deferred }> = [];
  const persistedFactors: ZoomFactor[] = [];
  const states: ZoomAuthorityState[] = [];
  const failures: ZoomAuthorityFailure[] = [];
  const authority = new ZoomAuthority(initialAuthorityState(), {
    applyWebviewZoom: (factor) => {
      const completion = deferred();
      applications.push({ factor, completion });
      return completion.promise;
    },
    persistPreference: (factor) => {
      persistedFactors.push(factor);
      return null;
    },
    publishState: (state) => states.push(state),
    onFailure: (failure) => failures.push(failure),
  });

  const firstRequestId = authority.requestZoom(1.1, { persist: true, reason: 'change' });
  const secondRequestId = authority.requestZoom(1.25, { persist: true, reason: 'change' });
  assert.ok(secondRequestId > firstRequestId);
  assert.equal(authority.getState().desiredFactor, 1.25);
  assert.deepEqual(applications.map((application) => application.factor), [1.1]);

  applications[0].completion.resolve();
  await flushMicrotasks();
  assert.deepEqual(applications.map((application) => application.factor), [1.1, 1.25]);
  assert.equal(states.some((state) => state.appliedFactor === 1.1), false);
  assert.deepEqual(authority.getState(), {
    desiredFactor: 1.25,
    appliedFactor: 1.1,
    persistedFactor: 1,
    latestRequestId: secondRequestId,
    pending: true,
  });
  assert.deepEqual(persistedFactors, []);

  applications[1].completion.resolve();
  await flushMicrotasks();
  assert.deepEqual(authority.getState(), {
    desiredFactor: 1.25,
    appliedFactor: 1.25,
    persistedFactor: 1.25,
    latestRequestId: secondRequestId,
    pending: false,
  });
  assert.deepEqual(persistedFactors, [1.25]);
  assert.deepEqual(failures, []);
});

test('restoring a persisted factor applies it without rewriting storage', async () => {
  const restoredInitialState = createInitialZoomAuthorityState({
    factor: 1.25,
    persistedFactor: 1.25,
    warning: null,
  });
  let persistenceCalls = 0;
  const authority = new ZoomAuthority(restoredInitialState, {
    applyWebviewZoom: async () => {},
    persistPreference: () => {
      persistenceCalls += 1;
      return null;
    },
    publishState: () => {},
    onFailure: () => {},
  });

  authority.requestZoom(1.25, { persist: false, reason: 'restore' });
  await flushMicrotasks();

  assert.equal(persistenceCalls, 0);
  assert.deepEqual(authority.getState(), {
    desiredFactor: 1.25,
    appliedFactor: 1.25,
    persistedFactor: 1.25,
    latestRequestId: 1,
    pending: false,
  });
});

test('webview failure never persists and stale failure cannot regress or report', async () => {
  const applications: Array<{ factor: ZoomFactor; completion: Deferred }> = [];
  const persistedFactors: ZoomFactor[] = [];
  const failures: ZoomAuthorityFailure[] = [];
  const authority = new ZoomAuthority(initialAuthorityState(), {
    applyWebviewZoom: (factor) => {
      const completion = deferred();
      applications.push({ factor, completion });
      return completion.promise;
    },
    persistPreference: (factor) => {
      persistedFactors.push(factor);
      return null;
    },
    publishState: () => {},
    onFailure: (failure) => failures.push(failure),
  });

  authority.requestZoom(1.1, { persist: true, reason: 'change' });
  authority.requestZoom(1.25, { persist: true, reason: 'change' });
  applications[0].completion.reject(new Error('stale failure'));
  await flushMicrotasks();
  applications[1].completion.reject(new Error('latest failure'));
  await flushMicrotasks();

  assert.deepEqual(persistedFactors, []);
  assert.equal(failures.length, 1);
  assert.equal(failures[0]?.kind, 'application');
  assert.match(String(failures[0]?.error), /latest failure/);
  assert.deepEqual(authority.getState(), {
    desiredFactor: 1,
    appliedFactor: 1,
    persistedFactor: 1,
    latestRequestId: 2,
    pending: false,
  });
});

test('latest failure reconciles to a successful intermediate webview factor', async () => {
  const applications: Deferred[] = [];
  const displayedFactors: ZoomFactor[] = [];
  const authority = new ZoomAuthority(initialAuthorityState(), {
    applyWebviewZoom: () => {
      const completion = deferred();
      applications.push(completion);
      return completion.promise;
    },
    persistPreference: () => null,
    publishState: (state) => displayedFactors.push(state.appliedFactor),
    onFailure: () => {},
  });

  authority.requestZoom(1.1, { persist: true, reason: 'change' });
  authority.requestZoom(1.25, { persist: true, reason: 'change' });
  applications[0].resolve();
  await flushMicrotasks();
  assert.equal(displayedFactors.includes(1.1), false);
  applications[1].reject(new Error('latest failure'));
  await flushMicrotasks();

  assert.equal(authority.getState().desiredFactor, 1.1);
  assert.equal(authority.getState().appliedFactor, 1.1);
  assert.equal(displayedFactors.at(-1), 1.1);
});

test('persistence failure keeps the successfully applied session zoom', async () => {
  const failures: ZoomAuthorityFailure[] = [];
  const authority = new ZoomAuthority(initialAuthorityState(), {
    applyWebviewZoom: async () => {},
    persistPreference: () => 'storage denied',
    publishState: () => {},
    onFailure: (failure) => failures.push(failure),
  });

  authority.requestZoom(1.4, { persist: true, reason: 'change' });
  await flushMicrotasks();

  assert.deepEqual(authority.getState(), {
    desiredFactor: 1.4,
    appliedFactor: 1.4,
    persistedFactor: 1,
    latestRequestId: 1,
    pending: false,
  });
  assert.deepEqual(failures, [{
    kind: 'persistence',
    factor: 1.4,
    reason: 'change',
    error: 'storage denied',
  }]);
});
